// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Keyframe editing model for the timeline property tree (layer-network-model
//! plan, Phase 4; REQ-LAYER-004).
//!
//! The timeline lists, per layer, the shell channel groups (Anchor Point /
//! Position / Scale
//! / Rotation / Opacity, plus Gain on audio layers) and every **network parameter that carries
//! keyframes** — node parameters of the layer's owned network whose
//! [`ParameterValue::Channel`]…[`ParameterValue::Channel4`] components hold a
//! [`ChannelSource::Keyframes`] source. That includes the In node's custom
//! parameters and subnet-promoted parameters (both are plain node parameters
//! of the layer network).
//!
//! Enumeration follows the network **into its subnets**, at any depth, because
//! evaluation does (AGENTS.md: layer networks are evaluated recursively through
//! the network boundary node). A flat listing beside a recursive evaluator is
//! what made keyframes vanish from the tree while the animation kept running
//! after a Collapse to Subnet. Rows are addressed by a bare [`NodeId`], which
//! is enough at any depth: ids come from one global counter, so an id names one
//! node in the whole hierarchy (REQ-LAYER-009).
//!
//! All editing functions take and return **layer-local frames**
//! (`comp_frame - start_frame + in_frame`, REQ-LAYER-006) and rebuild the
//! layer through the immutable graph API, so a whole edit lands in the
//! Document as one undo unit via `update_layer`.
//!
//! Removing the last keyframe of a channel reverts it to a constant holding
//! the removed key's value; a network parameter without any keyframed
//! component then drops out of the tree, mirroring the enumeration rule.

use std::sync::Arc;

use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::animation::curve::KeyframeCurve;
use ravel_core::animation::interpolation::Interpolation;
use ravel_core::animation::step::StepCurve;
use ravel_core::composition::Layer;
use ravel_core::graph::ParameterValue;
use ravel_core::id::NodeId;
use ravel_core::network as net;
use ravel_core::types::Vec2;

use crate::panels::timeline::PropertyGroup;

/// Identity of one property-tree row: a shell channel group or a network
/// parameter (`node` id + parameter key) of the layer's owned network,
/// including the networks its subnets own at any depth. The id needs no path:
/// a `NodeId` is unique across the whole document (REQ-LAYER-009).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropertyRowId {
    Shell(PropertyGroup),
    Network { node: NodeId, key: String },
}

/// One resolved property-tree row.
#[derive(Clone, Debug)]
pub struct PropertyRow {
    pub id: PropertyRowId,
    /// Display label for network rows (the In node's custom parameters show
    /// the bare key; other nodes show `"<label or type> · <key>"`), prefixed
    /// by the chain of enclosing subnet names for a row inside a subnet
    /// (`"Subnet 1 / Blur · radius"`). `None` for shell rows — the host
    /// localizes them (`timeline.property.*`).
    pub label: Option<String>,
    /// Per-component channel names, in component order.
    ///
    /// A name is either language-independent notation — the axis letters
    /// `X` / `Y` and the colour channels `R` / `G` / `B` / `A`, which the
    /// Timeline spec keeps untranslated — or a locale key for a component
    /// that is named by a word ([`CHANNEL_VALUE`] and the shell groups'
    /// `timeline.property.*` keys). The host translates the second kind at
    /// the display boundary and shows the first kind verbatim.
    pub channel_names: Vec<String>,
}

/// Locale key of the sole component of a single-channel parameter.
///
/// The other single-component rows are named after the property they belong
/// to (rotation, opacity, gain), so they reuse the shell group's own key; a
/// network parameter has no such word and is simply "the value".
pub const CHANNEL_VALUE: &str = "timeline.channel.value";

/// The tangent handle being edited on a keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TangentHandle {
    In,
    Out,
}

/// Whether dragging one tangent also mirrors the opposite handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TangentCoupling {
    /// Mirror the opposite handle (the default curve-editor gesture).
    Symmetric,
    /// Change only the dragged handle (the Alt-modified gesture).
    Separated,
}

/// Absolute tangent state emitted by one curve-editor drag preview.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyframeTangentEdit {
    pub frame: u64,
    pub handle: TangentHandle,
    pub tangent: Vec2,
    pub coupling: TangentCoupling,
}

/// Relative tangent edit applied to several selected keys in one channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyframeTangentDeltaEdit<'a> {
    pub frames: &'a [u64],
    pub handle: TangentHandle,
    pub delta: Vec2,
    pub coupling: TangentCoupling,
}

/// The layer-local frame for comp-frame UI (REQ-LAYER-006).
pub fn layer_local_frame(layer: &Layer, comp_frame: u64) -> u64 {
    layer.local_frame(comp_frame)
}

/// The comp-timeline frame a layer-local keyframe is displayed at
/// (`local - in + start`; can be negative when the key sits before `in`).
pub fn comp_frame_for_key(layer: &Layer, local_frame: u64) -> i64 {
    local_frame as i64 - layer.in_frame as i64 + layer.start_frame
}

/// The shell groups always shown in the tree, in display order.
///
/// The order is After Effects': Anchor Point, Position, Scale, Rotation,
/// Opacity — the order the reveal shortcuts (`A` / `P` / `S` / `R` / `T`)
/// address them in.
pub const SHELL_GROUPS: [PropertyGroup; 5] = [
    PropertyGroup::AnchorPoint,
    PropertyGroup::Position,
    PropertyGroup::Scale,
    PropertyGroup::Rotation,
    PropertyGroup::Opacity,
];

/// One After Effects-style *reveal* criterion: a property row is shown when it
/// matches at least one active criterion (`refactor-plan-0808.md`, unit 5).
///
/// The criteria filter the rows [`property_rows`] produces; they never change
/// which rows exist. A row hidden by a filter is hidden everywhere — the
/// header tree, the painter, hit testing, rubber-band selection and the
/// content height all read the same filtered list, because a filter that
/// reaches only some of them makes painting and hit testing disagree below
/// the first hidden row (`MED-APP-13`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RevealFilter {
    /// Rows that carry keyframes (AE `U`).
    Animated,
    /// One shell group (AE `A` / `P` / `S` / `R` / `T` / `L`).
    Group(PropertyGroup),
    /// Rows whose value differs from the shell default (AE `UU`).
    Modified,
    /// Rows driven by an expression (AE `EE`).
    Expression,
}

impl RevealFilter {
    /// Whether `row` of `layer` survives this criterion.
    pub fn matches(self, layer: &Layer, row: &PropertyRow) -> bool {
        let channels = || row_channels(layer, &row.id).unwrap_or_default();
        match self {
            Self::Group(group) => row.id == PropertyRowId::Shell(group),
            Self::Animated => channels()
                .iter()
                .any(|channel| matches!(channel.source, ChannelSource::Keyframes(_))),
            // A blend counts: the Properties badge calls such a channel
            // expression-driven, and one definition of "driven by an
            // expression" has to serve both panels.
            //
            // **Scope**: this can only reveal rows that exist, and a network
            // parameter earns a row by being keyframed
            // ([`property_rows`]). An expression attached to a parameter that
            // was never keyframed therefore has no row for `Alt+E` to keep.
            // Widening row generation is not this filter's job — it would
            // change the tree for everyone, not just while a filter is on.
            Self::Expression => channels().iter().any(|channel| {
                crate::properties::expression::source_has_expression(&channel.source)
            }),
            Self::Modified => match &row.id {
                PropertyRowId::Shell(group) => {
                    let defaults = shell_default_channels(*group);
                    let channels = channels();
                    channels.len() != defaults.len()
                        || channels
                            .iter()
                            .zip(&defaults)
                            .any(|(channel, default)| channel.source != default.source)
                }
                // A network parameter is only listed once it is keyframed, so
                // it is by construction no longer the processor's default.
                PropertyRowId::Network { .. } => true,
            },
        }
    }
}

/// The channels a shell group holds on a freshly created layer
/// ([`Layer::new`] and [`ravel_core::composition::AudioSource`]), which
/// [`RevealFilter::Modified`] compares against.
fn shell_default_channels(group: PropertyGroup) -> Vec<AnimationChannel> {
    let transform = ravel_core::composition::LayerTransform::default();
    match group {
        PropertyGroup::AnchorPoint => transform.anchor_point.to_vec(),
        PropertyGroup::Position => transform.position.to_vec(),
        PropertyGroup::Scale => transform.scale.to_vec(),
        PropertyGroup::Rotation => vec![transform.rotation],
        PropertyGroup::Opacity => vec![AnimationChannel::constant(1.0)],
        PropertyGroup::AudioGain => {
            vec![ravel_core::composition::AudioSource::default().gain]
        }
    }
}

/// The property-tree rows of a layer: the shell groups, then every network
/// parameter with at least one keyframed component (REQ-LAYER-004), ordered
/// deterministically by node id then parameter position — subnets included,
/// each subnet's rows following the node that owns them ([`network_rows`]).
pub fn property_rows(layer: &Layer) -> Vec<PropertyRow> {
    let mut rows: Vec<PropertyRow> = SHELL_GROUPS
        .iter()
        .map(|group| PropertyRow {
            id: PropertyRowId::Shell(*group),
            label: None,
            channel_names: shell_channel_names(*group)
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })
        .collect();

    if layer.audio.is_some() {
        rows.push(PropertyRow {
            id: PropertyRowId::Shell(PropertyGroup::AudioGain),
            label: None,
            channel_names: shell_channel_names(PropertyGroup::AudioGain)
                .iter()
                .map(|s| s.to_string())
                .collect(),
        });
    }

    network_rows(&layer.network, "", None, &mut rows);
    rows
}

/// Append the keyframed parameters of `graph` and of every subnet nested
/// inside it, node-id ordered at each level and each level's rows placed
/// directly after the subnet node that owns them.
///
/// `prefix` is the chain of enclosing subnet labels (`"Subnet 1 / "`). Without
/// it two identically named parameters in different subnets produce the same
/// row label and the tree stops being readable.
///
/// `owner` is the subnet node holding `graph`, and it decides one exclusion:
/// an inner In node's custom parameter under a key the owner **promotes** is
/// dead at evaluation time — `SubnetProcessor::process` binds the promoted
/// value and the inner In's own default is never read (REQ-LAYER-003) — so a
/// row for it would be a control that animates nothing. The layer-root In has
/// no owner and keeps all of its rows.
///
/// The recursion stops at any node without a `subnet`, and the hierarchy is a
/// tree of owned graphs, so no branch repeats: there is no depth limit, as
/// REQ-LAYER requires.
fn network_rows(
    graph: &ravel_core::graph::Graph,
    prefix: &str,
    owner: Option<&ravel_core::graph::Node>,
    rows: &mut Vec<PropertyRow>,
) {
    let mut nodes: Vec<_> = graph.nodes().collect();
    nodes.sort_by_key(|n| n.id);
    for node in nodes {
        let promoted_by_owner = |key: &str| {
            node.type_key == net::NET_IN_TYPE_KEY
                && owner.is_some_and(|owner| owner.parameters.iter().any(|p| p.key == key))
        };
        for param in &node.parameters {
            if promoted_by_owner(&param.key) {
                continue;
            }
            // An identifier parameter must never be animated (the Properties
            // keyframe toggle refuses it), so it has no keys for a row to
            // show. Refusing it here too means a document that carries one
            // anyway — hand-edited, or written by a future path that forgets
            // the rule — offers no Timeline gesture to grow it further.
            if ravel_core::composition::validate::is_identifier_parameter(
                &node.type_key,
                &param.key,
            ) {
                continue;
            }
            let Some(names) = keyframed_channel_names(&param.value) else {
                continue;
            };
            let label = if node.type_key == net::NET_IN_TYPE_KEY {
                format!("{prefix}{}", param.key)
            } else {
                let node_label = node.metadata.label.as_deref().unwrap_or(&node.type_key);
                format!("{prefix}{node_label} · {}", param.key)
            };
            rows.push(PropertyRow {
                id: PropertyRowId::Network {
                    node: node.id,
                    key: param.key.clone(),
                },
                label: Some(label),
                channel_names: names,
            });
        }
        if let Some(inner) = node.subnet.as_deref() {
            let node_label = node.metadata.label.as_deref().unwrap_or(&node.type_key);
            network_rows(inner, &format!("{prefix}{node_label} / "), Some(node), rows);
        }
    }
}

/// The component channels of a row, in component order. Resolves regardless
/// of whether the components currently hold keyframes (first-key insertion
/// works on constant channels). `None` when the row no longer resolves
/// (node or parameter deleted).
pub fn row_channels<'a>(layer: &'a Layer, id: &PropertyRowId) -> Option<Vec<&'a AnimationChannel>> {
    match id {
        PropertyRowId::Shell(group) => Some(shell_channels(layer, *group)),
        PropertyRowId::Network { node, key } => {
            let node_ref = layer.network.find_nested_node(*node)?;
            let param = node_ref.parameters.iter().find(|p| p.key == *key)?;
            channel_components(&param.value)
        }
    }
}

/// How the keys of a property row behave — the two questions the shared
/// Timeline gesture code cannot answer from an `AnimationChannel` alone.
///
/// It is deliberately not a second row list: every row is painted and hit
/// tested the same way, one lane of keys per component, and this only decides
/// which *affordances* a host offers on top of that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowValueKind {
    /// Continuous f32 channels. Interpolation modes and Bézier tangents apply.
    Float,
    /// f32 channels read back as rounded integers
    /// ([`ParameterValue::IntChannel`]). Editing is identical to `Float` —
    /// control points stay on the float grid — but the value a frame resolves
    /// to is rounded, which is why the curve editor draws it as a staircase.
    Integer,
    /// Held keys with no midpoint ([`ParameterValue::StringSteps`]). There is
    /// no interpolation to choose and no tangent to drag, and the curve editor
    /// never sees one (`ParameterValue::channels` answers `None`).
    Steps,
}

impl RowValueKind {
    /// Whether the row's keys are held steps, so interpolation and tangent
    /// editing do not apply to them.
    pub fn is_stepped(self) -> bool {
        self == Self::Steps
    }

    /// Whether the row's value is read back rounded to an integer, so a host
    /// that draws it must draw what evaluation resolves, not the float curve
    /// underneath.
    pub fn is_integral(self) -> bool {
        self == Self::Integer
    }
}

/// The value kind of a row. Shell rows and rows that no longer resolve are
/// `Float`: a layer shell holds nothing but f32 channels, and a stale row
/// offers no gesture for the kind to gate.
pub fn row_value_kind(layer: &Layer, id: &PropertyRowId) -> RowValueKind {
    match row_parameter_value(layer, id) {
        Some(ParameterValue::IntChannel(_)) => RowValueKind::Integer,
        Some(ParameterValue::StringSteps(_)) => RowValueKind::Steps,
        _ => RowValueKind::Float,
    }
}

/// Number of key lanes under a row — the length
/// [`PropertyRow::channel_names`] has, answered from a bare row id.
///
/// A step row has no `AnimationChannel` to count, so counting
/// [`row_channels`] would give it zero lanes and desynchronize a painter from
/// the hit test below the row.
pub fn row_component_count(layer: &Layer, id: &PropertyRowId) -> usize {
    if row_value_kind(layer, id).is_stepped() {
        return 1;
    }
    row_channels(layer, id).map_or(0, |channels| channels.len())
}

/// The layer-local frames carrying a key on one lane of a row, ascending.
///
/// Every host that enumerates keys — painter, hit test, navigator,
/// rubber-band selection, playhead snapping — reads them here instead of
/// peeling [`ChannelSource::Keyframes`] off itself, because a step row has
/// keys and no channel: the sites that peel the channel apart are exactly the
/// sites that would silently drop it.
pub fn row_key_frames(layer: &Layer, id: &PropertyRowId, component: usize) -> Vec<u64> {
    if let Some(ParameterValue::StringSteps(steps)) = row_parameter_value(layer, id) {
        if component != 0 {
            return Vec::new();
        }
        return steps.keys().iter().map(|key| key.frame).collect();
    }
    let Some(channels) = row_channels(layer, id) else {
        return Vec::new();
    };
    let Some(channel) = channels.get(component) else {
        return Vec::new();
    };
    match &channel.source {
        ChannelSource::Keyframes(curve) => curve.keyframes().iter().map(|key| key.frame).collect(),
        _ => Vec::new(),
    }
}

/// Snapshot of one row lane's keys, taken when a drag gesture starts.
///
/// The previews rebuild from the snapshot rather than from the document, so a
/// transient pass over an occupied frame during a live drag cannot
/// permanently merge two keys.
#[derive(Clone, Debug)]
pub enum RowKeys {
    Curve(KeyframeCurve),
    Steps(StepCurve<String>),
}

impl RowKeys {
    /// The float curve, for the gestures only a float row has: the value axis
    /// of a curve-editor drag, and tangent edits. `None` for a step row.
    pub fn curve(&self) -> Option<&KeyframeCurve> {
        match self {
            Self::Curve(curve) => Some(curve),
            Self::Steps(_) => None,
        }
    }
}

/// Snapshot one row lane's keys for a drag gesture. `None` when the row or
/// lane does not resolve, or holds no keys to move.
pub fn row_keys(layer: &Layer, id: &PropertyRowId, component: usize) -> Option<RowKeys> {
    if let Some(ParameterValue::StringSteps(steps)) = row_parameter_value(layer, id) {
        return (component == 0).then(|| RowKeys::Steps(steps.clone()));
    }
    let channels = row_channels(layer, id)?;
    match &channels.get(component)?.source {
        ChannelSource::Keyframes(curve) => Some(RowKeys::Curve(curve.clone())),
        _ => None,
    }
}

/// Gesture preview for moving several keys of one row lane by the same signed
/// frame delta: [`preview_keyframe_moves`] for a float row, its step-curve
/// twin for a step row.
///
/// Callers must clamp `frame_delta` so no destination frame is negative.
pub fn preview_row_key_moves(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &RowKeys,
    origin_frames: &[u64],
    frame_delta: i64,
) -> bool {
    match baseline {
        RowKeys::Curve(curve) => {
            preview_keyframe_moves(layer, id, component, curve, origin_frames, frame_delta)
        }
        RowKeys::Steps(steps) => {
            if component != 0 {
                return false;
            }
            let moving: Option<Vec<String>> = origin_frames
                .iter()
                .map(|frame| {
                    steps
                        .keys()
                        .iter()
                        .find(|key| key.frame == *frame)
                        .map(|key| key.value.clone())
                })
                .collect();
            let Some(moving) = moving else {
                return false;
            };
            mutate_step_curve(layer, id, |curve| {
                let mut rebuilt = steps.clone();
                for frame in origin_frames {
                    rebuilt.remove(*frame);
                }
                for (frame, value) in origin_frames.iter().zip(moving) {
                    rebuilt.insert((*frame as i64 + frame_delta) as u64, value);
                }
                *curve = rebuilt;
                true
            })
        }
    }
}

/// Whether the row has a key exactly at `frame` on `component`.
pub fn has_keyframe_at(layer: &Layer, id: &PropertyRowId, component: usize, frame: u64) -> bool {
    if let Some(ParameterValue::StringSteps(steps)) = row_parameter_value(layer, id) {
        return component == 0 && steps.contains_key(frame);
    }
    let Some(channels) = row_channels(layer, id) else {
        return false;
    };
    let Some(channel) = channels.get(component) else {
        return false;
    };
    match &channel.source {
        ChannelSource::Keyframes(curve) => curve.keyframes().iter().any(|k| k.frame == frame),
        _ => false,
    }
}

/// Insert (or overwrite) a keyframe at `frame` holding the row's current
/// value at `frame`. A constant channel is converted to keyframes, keeping
/// its value as the curve's default; a step row re-keys the string it already
/// holds there. Returns `false` when the row or component does not resolve.
pub fn insert_keyframe(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
) -> bool {
    if row_value_kind(layer, id).is_stepped() {
        return component == 0
            && mutate_step_curve(layer, id, |curve| {
                let held = curve.sample(frame as f64).clone();
                curve.insert(frame, held);
                true
            });
    }
    mutate_channel(layer, id, component, |channel| {
        let value = channel_value(channel, frame);
        match &mut channel.source {
            ChannelSource::Constant(v) => {
                let mut curve = KeyframeCurve::with_default(*v);
                curve.insert(frame, value, Interpolation::Linear);
                channel.source = ChannelSource::Keyframes(curve);
            }
            ChannelSource::Keyframes(curve) => {
                set_curve_value(curve, frame, value);
            }
            // Expressions / node-output bindings / blends are not key-editable.
            _ => return false,
        }
        true
    })
}

/// Remove the keyframe at `frame`. When the curve becomes empty the channel
/// reverts to a constant holding the removed key's value (a fully constant
/// network parameter then drops out of the property tree); an emptied step
/// curve re-types the parameter back to a plain `String` holding the curve's
/// default. Returns `false` when no keyframe exists at `frame`.
pub fn remove_keyframe(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
) -> bool {
    if row_value_kind(layer, id).is_stepped() {
        return component == 0
            && mutate_step_curve(layer, id, |curve| curve.remove(frame).is_some());
    }
    mutate_channel(layer, id, component, |channel| {
        let ChannelSource::Keyframes(curve) = &mut channel.source else {
            return false;
        };
        let Some(removed) = curve.remove(frame) else {
            return false;
        };
        if curve.is_empty() {
            channel.source = ChannelSource::Constant(removed.value);
        }
        true
    })
}

/// Move the keyframe at `from` to `to`, preserving value and tangents (an
/// existing keyframe at `to` is overwritten). Returns `false` when no
/// keyframe exists at `from`.
pub fn move_keyframe(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    from: u64,
    to: u64,
) -> bool {
    if row_value_kind(layer, id).is_stepped() {
        return component == 0 && mutate_step_curve(layer, id, |curve| curve.move_key(from, to));
    }
    mutate_channel(layer, id, component, |channel| {
        let ChannelSource::Keyframes(curve) = &mut channel.source else {
            return false;
        };
        curve.move_keyframe(from, to)
    })
}

/// The value [`set_channel_value`] would edit at `frame`: the constant, or
/// the curve sampled there.
///
/// `None` means *there is no editable value here* — the row or component
/// does not resolve, or the source is one `set_channel_value` refuses (an
/// expression, a blend, a node-output binding). A caller that offers an
/// editing control reads this first, so it never shows a number the write
/// path would silently drop.
pub fn channel_value_at(
    layer: &Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
) -> Option<f32> {
    let channels = row_channels(layer, id)?;
    match &channels.get(component)?.source {
        ChannelSource::Constant(value) => Some(*value),
        ChannelSource::Keyframes(curve) => Some(curve.sample(frame as f64)),
        _ => None,
    }
}

/// Set the channel's value at `frame`: a keyframed channel gets an updated
/// key (preserving its interpolation and tangents) or an inserted one; a
/// constant channel has its constant replaced. Returns `false` when the row
/// or component does not resolve or the source is not key-editable.
pub fn set_channel_value(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
    value: f32,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        match &mut channel.source {
            ChannelSource::Constant(v) => *v = value,
            ChannelSource::Keyframes(curve) => {
                set_curve_value(curve, frame, value);
            }
            _ => return false,
        }
        true
    })
}

/// Write `value` at `frame`, keeping an existing key's interpolation mode
/// and tangents (a fresh key is Linear with zero tangents).
pub fn set_curve_value(curve: &mut KeyframeCurve, frame: u64, value: f32) {
    if !curve.modify(frame, value, None) {
        curve.insert(frame, value, Interpolation::Linear);
    }
}

/// Set one keyframe tangent, clamping newly saved handle x offsets to their
/// adjacent keyframe intervals.
///
/// Tangents are offsets in `(frame, value)` space. Incoming handles therefore
/// have a non-positive x offset and outgoing handles a non-negative one. In
/// [`TangentCoupling::Symmetric`] mode the opposite handle is mirrored and
/// independently clamped to its own adjacent interval; if that interval is
/// shorter, its y offset scales by the same ratio to keep both handles
/// collinear. Existing tangents on other keys are never normalized; moving a
/// neighbor can therefore shrink an interval without destructively changing a
/// previously saved handle.
pub fn set_curve_tangent(
    curve: &mut KeyframeCurve,
    frame: u64,
    handle: TangentHandle,
    tangent: Vec2,
    coupling: TangentCoupling,
) -> bool {
    let Some(index) = curve.keyframes().iter().position(|key| key.frame == frame) else {
        return false;
    };
    let previous_frame = index
        .checked_sub(1)
        .map(|previous| curve.keyframes()[previous].frame);
    let next_frame = curve.keyframes().get(index + 1).map(|next| next.frame);
    let mut keyframe = curve.keyframes()[index];
    let saved = clamp_tangent(tangent, handle, frame, previous_frame, next_frame);

    match handle {
        TangentHandle::In => keyframe.tangent_in = saved,
        TangentHandle::Out => keyframe.tangent_out = saved,
    }
    if coupling == TangentCoupling::Symmetric {
        let opposite = match handle {
            TangentHandle::In => TangentHandle::Out,
            TangentHandle::Out => TangentHandle::In,
        };
        let requested_mirror = Vec2(-saved.0, -saved.1);
        let mut mirrored = clamp_tangent(
            requested_mirror,
            opposite,
            frame,
            previous_frame,
            next_frame,
        );
        if requested_mirror.0 != 0.0 && mirrored.0 != requested_mirror.0 {
            mirrored.1 *= (mirrored.0 / requested_mirror.0).abs();
        }
        match opposite {
            TangentHandle::In => keyframe.tangent_in = mirrored,
            TangentHandle::Out => keyframe.tangent_out = mirrored,
        }
    }

    curve.insert_keyframe(keyframe);
    true
}

/// Set a keyframe's interpolation mode without changing its value or tangent
/// values. The interpolation stored on a key controls the segment after it.
pub fn set_curve_interpolation(
    curve: &mut KeyframeCurve,
    frame: u64,
    interpolation: Interpolation,
) -> bool {
    let Some(index) = curve
        .keyframes()
        .iter()
        .position(|keyframe| keyframe.frame == frame)
    else {
        return false;
    };
    let mut keyframe = curve.keyframes()[index];
    let mut next = curve.keyframes().get(index + 1).copied();

    // Fresh Linear/Step keys have zero-length tangents. When their outgoing
    // segment becomes Bezier, seed one-third handles along the same straight
    // line. The curve is visually unchanged, but both controls become
    // immediately grabbable. Previously edited non-zero tangents survive mode
    // switches verbatim.
    if interpolation == Interpolation::Bezier
        && keyframe.interpolation != Interpolation::Bezier
        && let Some(next_keyframe) = &mut next
    {
        let third = 1.0 / 3.0;
        let frame_delta = (next_keyframe.frame - keyframe.frame) as f32 * third;
        let value_delta = (next_keyframe.value - keyframe.value) * third;
        if keyframe.tangent_out == Vec2(0.0, 0.0) {
            keyframe.tangent_out = Vec2(frame_delta, value_delta);
        }
        if next_keyframe.tangent_in == Vec2(0.0, 0.0) {
            next_keyframe.tangent_in = Vec2(-frame_delta, -value_delta);
        }
    }
    keyframe.interpolation = interpolation;
    curve.insert_keyframe(keyframe);
    if let Some(next) = next {
        curve.insert_keyframe(next);
    }
    true
}

/// Set a tangent on one layer property channel. Network parameters are rebuilt
/// through the immutable graph path used by the other keyframe operations.
pub fn set_keyframe_tangent(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
    handle: TangentHandle,
    tangent: Vec2,
    coupling: TangentCoupling,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let ChannelSource::Keyframes(curve) = &mut channel.source else {
            return false;
        };
        set_curve_tangent(curve, frame, handle, tangent, coupling)
    })
}

/// Set the interpolation mode of one keyframe on a layer property channel.
pub fn set_keyframe_interpolation(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
    interpolation: Interpolation,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let ChannelSource::Keyframes(curve) = &mut channel.source else {
            return false;
        };
        set_curve_interpolation(curve, frame, interpolation)
    })
}

/// Gesture preview for a keyframe drag: restore `baseline` (the curve as it
/// was when the gesture started) and move its key from `origin_frame` to
/// `new_frame`. Deriving every preview from the pre-gesture curve means a
/// transient pass over an occupied frame does not permanently merge the two
/// keys — only the committed end position can overwrite. Returns `false`
/// when the row/component no longer resolves or the baseline has no key at
/// `origin_frame`.
pub fn preview_keyframe_move(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &KeyframeCurve,
    origin_frame: u64,
    new_frame: u64,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let mut curve = baseline.clone();
        if !curve.move_keyframe(origin_frame, new_frame) {
            return false;
        }
        channel.source = ChannelSource::Keyframes(curve);
        true
    })
}

/// Gesture preview for moving several keyframes in one channel by the same
/// signed frame delta. The preview always rebuilds from `baseline`, removing
/// all moving keys before inserting their shifted copies, so crossing an
/// occupied frame during a live drag cannot permanently discard a key.
///
/// Returns `false` when the row/component no longer resolves or any requested
/// source frame is absent from the baseline. Callers must clamp `delta` so no
/// destination frame is negative.
pub fn preview_keyframe_moves(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &KeyframeCurve,
    origin_frames: &[u64],
    delta: i64,
) -> bool {
    preview_keyframe_moves_with_value_delta(
        layer,
        id,
        component,
        baseline,
        origin_frames,
        delta,
        0.0,
    )
}

/// Gesture preview for moving several keyframes in frame/value space.
/// Every preview rebuilds from `baseline`, so transient collisions and
/// modifier changes do not accumulate across mouse-move events.
pub fn preview_keyframe_moves_with_value_delta(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &KeyframeCurve,
    origin_frames: &[u64],
    frame_delta: i64,
    value_delta: f32,
) -> bool {
    let moving = origin_frames
        .iter()
        .map(|frame| {
            baseline
                .keyframes()
                .iter()
                .find(|keyframe| keyframe.frame == *frame)
                .cloned()
        })
        .collect::<Option<Vec<_>>>();
    let Some(moving) = moving else {
        return false;
    };

    mutate_channel(layer, id, component, |channel| {
        let mut curve = baseline.clone();
        for frame in origin_frames {
            curve.remove(*frame);
        }
        for mut keyframe in moving {
            keyframe.frame = (keyframe.frame as i64 + frame_delta) as u64;
            keyframe.value += value_delta;
            curve.insert_keyframe(keyframe);
        }
        channel.source = ChannelSource::Keyframes(curve);
        true
    })
}

/// Gesture preview for one tangent edit. Rebuilding from `baseline` keeps
/// Alt coupling changes reversible while the drag is still live.
pub fn preview_keyframe_tangent(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &KeyframeCurve,
    edit: KeyframeTangentEdit,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let mut curve = baseline.clone();
        if !set_curve_tangent(
            &mut curve,
            edit.frame,
            edit.handle,
            edit.tangent,
            edit.coupling,
        ) {
            return false;
        }
        channel.source = ChannelSource::Keyframes(curve);
        true
    })
}

/// Gesture preview for the same tangent delta across selected keyframes.
/// Keys without an active handle on `edit.handle` are skipped.
pub fn preview_keyframe_tangents_with_delta(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &KeyframeCurve,
    edit: KeyframeTangentDeltaEdit<'_>,
) -> bool {
    let targets: Vec<_> = edit
        .frames
        .iter()
        .filter_map(|frame| {
            let index = baseline
                .keyframes()
                .iter()
                .position(|keyframe| keyframe.frame == *frame)?;
            let applicable = match edit.handle {
                TangentHandle::In => {
                    index > 0
                        && baseline.keyframes()[index - 1].interpolation == Interpolation::Bezier
                }
                TangentHandle::Out => {
                    index + 1 < baseline.len()
                        && baseline.keyframes()[index].interpolation == Interpolation::Bezier
                }
            };
            if !applicable {
                return None;
            }
            let keyframe = baseline.keyframes()[index];
            let original = match edit.handle {
                TangentHandle::In => keyframe.tangent_in,
                TangentHandle::Out => keyframe.tangent_out,
            };
            Some((
                *frame,
                Vec2(original.0 + edit.delta.0, original.1 + edit.delta.1),
            ))
        })
        .collect();
    if targets.is_empty() {
        return false;
    }

    mutate_channel(layer, id, component, |channel| {
        let mut curve = baseline.clone();
        for (frame, tangent) in &targets {
            set_curve_tangent(&mut curve, *frame, edit.handle, *tangent, edit.coupling);
        }
        channel.source = ChannelSource::Keyframes(curve);
        true
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn clamp_tangent(
    tangent: Vec2,
    handle: TangentHandle,
    frame: u64,
    previous_frame: Option<u64>,
    next_frame: Option<u64>,
) -> Vec2 {
    let x = match handle {
        TangentHandle::In => {
            let minimum = previous_frame
                .map(|previous| -((frame - previous) as f32))
                .unwrap_or(f32::NEG_INFINITY);
            tangent.0.clamp(minimum, 0.0)
        }
        TangentHandle::Out => {
            let maximum = next_frame
                .map(|next| (next - frame) as f32)
                .unwrap_or(f32::INFINITY);
            tangent.0.clamp(0.0, maximum)
        }
    };
    Vec2(x, tangent.1)
}

/// The value a channel holds at `frame` without an evaluation context:
/// the constant, the curve sample, or 0 for unresolvable sources.
fn channel_value(channel: &AnimationChannel, frame: u64) -> f32 {
    match &channel.source {
        ChannelSource::Constant(v) => *v,
        ChannelSource::Keyframes(curve) => curve.sample(frame as f64),
        _ => 0.0,
    }
}

fn shell_channels(layer: &Layer, group: PropertyGroup) -> Vec<&AnimationChannel> {
    match group {
        PropertyGroup::Position => {
            vec![&layer.transform.position[0], &layer.transform.position[1]]
        }
        PropertyGroup::Scale => vec![&layer.transform.scale[0], &layer.transform.scale[1]],
        PropertyGroup::Rotation => vec![&layer.transform.rotation],
        PropertyGroup::Opacity => vec![&layer.opacity],
        PropertyGroup::AudioGain => layer
            .audio
            .as_ref()
            .map(|audio| vec![&audio.gain])
            .unwrap_or_default(),
        PropertyGroup::AnchorPoint => {
            vec![
                &layer.transform.anchor_point[0],
                &layer.transform.anchor_point[1],
            ]
        }
    }
}

/// Display names of a shell group's component channels
/// ([`PropertyRow::channel_names`]: axis letters verbatim, words as locale
/// keys).
pub fn shell_channel_names(group: PropertyGroup) -> &'static [&'static str] {
    match group {
        PropertyGroup::Position | PropertyGroup::Scale | PropertyGroup::AnchorPoint => &["X", "Y"],
        PropertyGroup::Rotation => &["timeline.property.rotation"],
        PropertyGroup::Opacity => &["timeline.property.opacity"],
        PropertyGroup::AudioGain => &["timeline.property.gain"],
    }
}

/// The component channels of a `Channel*` parameter value (`None` for
/// non-animatable variants — `Int` / `Bool` are constant-only in v1,
/// REQ-LAYER-004).
fn channel_components(value: &ParameterValue) -> Option<Vec<&AnimationChannel>> {
    match value {
        // An animatable int carries the same f32 channel, so every read and
        // write below works on it unchanged — the rounding back to `i32`
        // happens where the value is read.
        ParameterValue::Channel(ch) | ParameterValue::IntChannel(ch) => Some(vec![ch]),
        ParameterValue::Channel2(chs) => Some(chs.iter().collect()),
        ParameterValue::Channel3(chs) => Some(chs.iter().collect()),
        ParameterValue::Channel4(chs) => Some(chs.iter().collect()),
        _ => None,
    }
}

/// Component names when the parameter carries keys — a `Channel*` value with
/// at least one keyframed component, or a non-empty step curve (`None` = not
/// part of the property tree).
fn keyframed_channel_names(value: &ParameterValue) -> Option<Vec<String>> {
    // A step curve has no float components at all, so it never reaches
    // `channel_components`; one row with one lane of held keys is its whole
    // shape ([`RowValueKind::Steps`]).
    if let ParameterValue::StringSteps(steps) = value {
        return (!steps.is_empty()).then(|| vec![CHANNEL_VALUE.to_string()]);
    }
    let components = channel_components(value)?;
    if !components
        .iter()
        .any(|ch| matches!(ch.source, ChannelSource::Keyframes(_)))
    {
        return None;
    }
    let names = match components.len() {
        1 => vec![CHANNEL_VALUE],
        2 => vec!["X", "Y"],
        3 => vec!["R", "G", "B"],
        _ => vec!["R", "G", "B", "A"],
    };
    Some(names.into_iter().map(str::to_string).collect())
}

/// The parameter value behind a network row (`None` for a shell row or a row
/// that no longer resolves).
fn row_parameter_value<'a>(layer: &'a Layer, id: &PropertyRowId) -> Option<&'a ParameterValue> {
    let PropertyRowId::Network { node, key } = id else {
        return None;
    };
    let node_ref = layer.network.find_nested_node(*node)?;
    node_ref
        .parameters
        .iter()
        .find(|p| p.key == *key)
        .map(|p| &p.value)
}

/// Apply `f` to a row's step curve, rebuilding the owning node so the layer's
/// immutable graph stays consistent — the step-curve twin of
/// [`mutate_channel`].
///
/// A curve `f` leaves empty re-types the parameter back to a plain `String`
/// holding the curve's **default**, which is the round trip the Properties
/// keyframe toggle performs: the default is the constant the parameter had
/// before it was keyed, while the last key removed is whatever the user
/// happened to edit last.
fn mutate_step_curve(
    layer: &mut Layer,
    id: &PropertyRowId,
    f: impl FnOnce(&mut StepCurve<String>) -> bool,
) -> bool {
    let PropertyRowId::Network { node, key } = id else {
        return false;
    };
    let Some(node_ref) = layer.network.find_nested_node(*node) else {
        return false;
    };
    let mut updated = (**node_ref).clone();
    let Some(param) = updated.parameters.iter_mut().find(|p| p.key == *key) else {
        return false;
    };
    let ParameterValue::StringSteps(steps) = &mut param.value else {
        return false;
    };
    if !f(steps) {
        return false;
    }
    if steps.is_empty() {
        param.value = ParameterValue::String(steps.default_value().clone());
    }
    // Only a parameter value changed, so no pin moved and the subnets on the
    // way back up keep the interface they had.
    let Some(network) = layer.network.replace_nested_node(Arc::new(updated)) else {
        return false;
    };
    layer.network = network;
    true
}

/// Apply `f` to the channel at `component`, rebuilding the owning node for
/// network rows so the layer's immutable graph stays consistent.
fn mutate_channel(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    f: impl FnOnce(&mut AnimationChannel) -> bool,
) -> bool {
    match id {
        PropertyRowId::Shell(group) => {
            let channel = match group {
                PropertyGroup::Position => layer.transform.position.get_mut(component),
                PropertyGroup::Scale => layer.transform.scale.get_mut(component),
                PropertyGroup::Rotation => {
                    (component == 0).then_some(&mut layer.transform.rotation)
                }
                PropertyGroup::Opacity => (component == 0).then_some(&mut layer.opacity),
                PropertyGroup::AudioGain => (component == 0)
                    .then(|| layer.audio.as_mut().map(|audio| &mut audio.gain))
                    .flatten(),
                PropertyGroup::AnchorPoint => layer.transform.anchor_point.get_mut(component),
            };
            let Some(channel) = channel else {
                return false;
            };
            f(channel)
        }
        PropertyRowId::Network { node, key } => {
            let Some(node_ref) = layer.network.find_nested_node(*node) else {
                return false;
            };
            let mut updated = (**node_ref).clone();
            let Some(param) = updated.parameters.iter_mut().find(|p| p.key == *key) else {
                return false;
            };
            let channel = match &mut param.value {
                ParameterValue::Channel(ch) | ParameterValue::IntChannel(ch) if component == 0 => {
                    Some(ch)
                }
                ParameterValue::Channel2(chs) => chs.get_mut(component),
                ParameterValue::Channel3(chs) => chs.get_mut(component),
                ParameterValue::Channel4(chs) => chs.get_mut(component),
                _ => None,
            };
            let Some(channel) = channel else {
                return false;
            };
            if !f(channel) {
                return false;
            }
            // The node may live in a subnet; only a parameter VALUE changed,
            // so no pin moved and the subnets on the way back up keep the
            // interface they had.
            let Some(network) = layer.network.replace_nested_node(Arc::new(updated)) else {
                return false;
            };
            layer.network = network;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{DataTypeId, LayerId};
    use ravel_core::types::FrameRate;

    fn curve_0_to_10() -> KeyframeCurve {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        curve
    }

    fn eval_ctx() -> ravel_core::eval::EvalContext {
        ravel_core::eval::EvalContext::new(0, FrameRate::new(30, 1), (16, 16))
    }

    /// Layer with an In custom parameter `amount` (constant) and a node
    /// `blur` whose `radius` is keyframed.
    fn test_layer() -> Layer {
        let in_node = Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(2.0));
        let blur = Node::new(NodeId::new(20), "blur")
            .with_param(
                "radius",
                ParameterValue::Channel(AnimationChannel::keyframes(curve_0_to_10())),
            )
            .with_param("mix", ParameterValue::Float(0.5));
        let network = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(blur)
            .unwrap();
        Layer::new(LayerId::new(1), "L", network).with_time(10, 5, 300)
    }

    #[test]
    fn local_frame_conversion_roundtrips() {
        let layer = test_layer(); // start 10, in 5
        assert_eq!(layer_local_frame(&layer, 15), 10); // 15 - 10 + 5
        assert_eq!(comp_frame_for_key(&layer, 10), 15);
        assert_eq!(layer_local_frame(&layer, 0), 0, "clamped at zero");
    }

    #[test]
    fn rows_list_shell_groups_then_keyframed_network_params() {
        let rows = property_rows(&test_layer());
        assert_eq!(rows.len(), 6);
        // After Effects' order: Anchor Point, Position, Scale, Rotation,
        // Opacity, then the keyframed network parameters.
        assert_eq!(rows[0].id, PropertyRowId::Shell(PropertyGroup::AnchorPoint));
        assert_eq!(rows[1].id, PropertyRowId::Shell(PropertyGroup::Position));
        assert_eq!(rows[4].id, PropertyRowId::Shell(PropertyGroup::Opacity));
        assert_eq!(
            rows[5].id,
            PropertyRowId::Network {
                node: NodeId::new(20),
                key: "radius".into()
            }
        );
        assert_eq!(rows[5].label.as_deref(), Some("blur · radius"));
        // Constant-only params (Float `mix`, `amount`) are not listed.
        assert!(!rows.iter().any(|r| matches!(
            &r.id,
            PropertyRowId::Network { key, .. } if key == "mix" || key == "amount"
        )));
    }

    /// Anchor Point is a row of every layer's tree, and it is keyable through
    /// it like any other shell group (AE's `A`).
    #[test]
    fn anchor_point_is_a_keyable_shell_row() {
        let mut layer = test_layer();
        let row = PropertyRowId::Shell(PropertyGroup::AnchorPoint);
        let listed = property_rows(&layer);
        assert_eq!(listed[0].id, row);
        assert_eq!(listed[0].channel_names, vec!["X", "Y"]);

        assert!(insert_keyframe(&mut layer, &row, 1, 3));
        assert!(has_keyframe_at(&layer, &row, 1, 3));
        assert!(set_channel_value(&mut layer, &row, 1, 3, 12.0));
        assert!(
            (layer.transform.anchor_point[1].evaluate(3.0, &eval_ctx()) - 12.0).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn audio_gain_is_a_shell_keyframe_row_only_on_audio_layers() {
        let mut layer = test_layer();
        assert!(
            !property_rows(&layer)
                .iter()
                .any(|row| { row.id == PropertyRowId::Shell(PropertyGroup::AudioGain) })
        );

        layer.audio = Some(ravel_core::composition::AudioSource::new(
            ravel_core::id::AssetId::next(),
            0,
        ));
        let row = PropertyRowId::Shell(PropertyGroup::AudioGain);
        assert!(
            property_rows(&layer)
                .iter()
                .any(|candidate| candidate.id == row)
        );
        assert!(insert_keyframe(&mut layer, &row, 0, 7));
        assert!(has_keyframe_at(&layer, &row, 0, 7));
        assert!(set_channel_value(&mut layer, &row, 0, 7, 0.25));
        assert!(
            (layer
                .audio
                .as_ref()
                .unwrap()
                .gain
                .evaluate(7.0, &eval_ctx())
                - 0.25)
                .abs()
                < f32::EPSILON
        );
    }

    /// The read side of [`set_channel_value`]: a value exists exactly where
    /// the write path would accept one.
    #[test]
    fn channel_value_at_reads_only_editable_sources() {
        let mut layer = test_layer();
        let position = PropertyRowId::Shell(PropertyGroup::Position);
        let radius = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };

        // Constant channel: the constant, at any frame.
        assert_eq!(channel_value_at(&layer, &position, 0, 42), Some(0.0));
        // Keyframed channel: the curve sampled at the layer-local frame.
        assert_eq!(channel_value_at(&layer, &radius, 0, 0), Some(0.0));
        assert_eq!(channel_value_at(&layer, &radius, 0, 10), Some(1.0));
        let half = channel_value_at(&layer, &radius, 0, 5).expect("sampled mid-curve");
        assert!((half - 0.5).abs() < 1e-4, "linear sample at 5: {half}");

        // Out-of-range component and unknown row resolve to nothing.
        assert_eq!(channel_value_at(&layer, &position, 2, 0), None);
        assert_eq!(
            channel_value_at(
                &layer,
                &PropertyRowId::Network {
                    node: NodeId::new(99),
                    key: "gone".into()
                },
                0,
                0
            ),
            None
        );

        // An expression-driven channel has no editable value: the write path
        // refuses it, so the read path must not offer a number either.
        layer.transform.position[0] = AnimationChannel::new(ChannelSource::Expression(
            ravel_core::animation::ParameterExpression::new("frame * 2"),
        ));
        assert_eq!(channel_value_at(&layer, &position, 0, 3), None);
        assert!(!set_channel_value(&mut layer, &position, 0, 3, 1.0));
    }

    #[test]
    fn insert_keyframe_converts_a_constant_custom_param() {
        let mut layer = test_layer();
        let in_id = PropertyRowId::Network {
            node: NodeId::new(10),
            key: "amount".into(),
        };
        // Give the custom param a constant channel first (the properties
        // toggle does this conversion; `Float` stays constant-only here).
        let in_node = layer.network.node(NodeId::new(10)).unwrap();
        let mut updated = (**in_node).clone();
        updated
            .parameters
            .iter_mut()
            .find(|p| p.key == "amount")
            .unwrap()
            .value = ParameterValue::Channel(AnimationChannel::constant(2.0));
        layer.network = layer.network.clone().replace_node(Arc::new(updated));

        assert!(insert_keyframe(&mut layer, &in_id, 0, 7));
        // The constant 2.0 became a keyframed channel keyed at frame 7.
        let channels = row_channels(&layer, &in_id).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 1);
        assert!((curve.sample(7.0) - 2.0).abs() < f32::EPSILON);
        // …and the param now shows up in the tree with the In bare-key label.
        let row = property_rows(&layer)
            .into_iter()
            .find(|r| r.id == in_id)
            .expect("keyframed custom param listed");
        assert_eq!(row.label.as_deref(), Some("amount"));
    }

    #[test]
    fn insert_on_keyframed_channel_samples_the_current_value() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        assert!(insert_keyframe(&mut layer, &row, 0, 5));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 3);
        assert!(
            (curve.sample(5.0) - 0.5).abs() < 1e-4,
            "interpolated value kept"
        );
    }

    #[test]
    fn remove_last_keyframe_reverts_to_constant_and_drops_the_row() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        assert!(remove_keyframe(&mut layer, &row, 0, 0));
        assert!(remove_keyframe(&mut layer, &row, 0, 10));
        assert!(!remove_keyframe(&mut layer, &row, 0, 10), "already gone");
        let channels = row_channels(&layer, &row).unwrap();
        assert_eq!(channels[0].source, ChannelSource::Constant(1.0));
        assert!(!property_rows(&layer).iter().any(|r| r.id == row));
    }

    #[test]
    fn move_keyframe_preserves_the_value() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        assert!(move_keyframe(&mut layer, &row, 0, 10, 20));
        assert!(has_keyframe_at(&layer, &row, 0, 20));
        assert!(!has_keyframe_at(&layer, &row, 0, 10));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert!((curve.sample(20.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn shell_channels_are_editable() {
        let mut layer = test_layer();
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        // Constant → keyframe at the current value.
        assert!(insert_keyframe(&mut layer, &row, 0, 4));
        assert!(has_keyframe_at(&layer, &row, 0, 4));
        // Scrub-style set keys the animated channel.
        assert!(set_channel_value(&mut layer, &row, 0, 4, 12.0));
        assert!(set_channel_value(&mut layer, &row, 0, 8, 20.0));
        let ctx = eval_ctx();
        assert!((layer.transform.position[0].evaluate(4.0, &ctx) - 12.0).abs() < f32::EPSILON);
        assert!((layer.transform.position[0].evaluate(8.0, &ctx) - 20.0).abs() < f32::EPSILON);
        // Out-of-range component is rejected.
        assert!(!set_channel_value(
            &mut layer,
            &PropertyRowId::Shell(PropertyGroup::Rotation),
            1,
            0,
            1.0
        ));
    }

    #[test]
    fn missing_node_or_param_is_rejected() {
        let mut layer = test_layer();
        let bogus_node = PropertyRowId::Network {
            node: NodeId::new(999),
            key: "x".into(),
        };
        assert!(!insert_keyframe(&mut layer, &bogus_node, 0, 0));
        let bogus_key = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "nope".into(),
        };
        assert!(!insert_keyframe(&mut layer, &bogus_key, 0, 0));
        // Int params are constant-only in v1 (REQ-LAYER-004).
        let float_key = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "mix".into(),
        };
        assert!(!insert_keyframe(&mut layer, &float_key, 0, 0));
    }

    #[test]
    fn multi_component_params_report_component_names() {
        let color = Node::new(NodeId::new(30), "constant.color").with_param(
            "color",
            ParameterValue::Channel4([
                AnimationChannel::keyframes(curve_0_to_10()),
                AnimationChannel::constant(0.5),
                AnimationChannel::constant(0.5),
                AnimationChannel::constant(1.0),
            ]),
        );
        let network = Graph::new().add_node(color).unwrap();
        let layer = Layer::new(LayerId::new(2), "C", network).with_time(0, 0, 100);
        let rows = property_rows(&layer);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[5].channel_names, vec!["R", "G", "B", "A"]);
        // Per-component editing targets the keyframed component only.
        let mut layer = layer;
        let row = rows[5].id.clone();
        assert!(insert_keyframe(&mut layer, &row, 1, 3));
        assert!(has_keyframe_at(&layer, &row, 1, 3));
        assert!(!has_keyframe_at(&layer, &row, 2, 3));
    }

    /// Drag previews derive from the gesture baseline: passing over an
    /// occupied frame must not destroy the other key.
    #[test]
    fn preview_move_across_a_collision_restores_the_other_key() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        }; // keys at 0 and 10
        let baseline = {
            let channels = row_channels(&layer, &row).unwrap();
            let ChannelSource::Keyframes(curve) = &channels[0].source else {
                panic!("expected keyframes");
            };
            curve.clone()
        };

        // Drag 0 → 10 (overwrites the key at 10 in the preview)…
        assert!(preview_keyframe_move(&mut layer, &row, 0, &baseline, 0, 10));
        // …then keep going to 20: the frame-10 key is restored, not merged.
        assert!(preview_keyframe_move(&mut layer, &row, 0, &baseline, 0, 20));
        assert!(has_keyframe_at(&layer, &row, 0, 10));
        assert!(has_keyframe_at(&layer, &row, 0, 20));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 2);
        assert!((curve.sample(10.0) - 1.0).abs() < f32::EPSILON);
        assert!((curve.sample(20.0) - 0.0).abs() < f32::EPSILON);
        // Releasing on the occupied frame does overwrite (end position).
        assert!(preview_keyframe_move(&mut layer, &row, 0, &baseline, 0, 10));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 1);
    }

    #[test]
    fn graph_preview_moves_frames_and_values_from_the_baseline() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        let baseline = {
            let channels = row_channels(&layer, &row).unwrap();
            let ChannelSource::Keyframes(curve) = &channels[0].source else {
                panic!("expected keyframes");
            };
            curve.clone()
        };

        assert!(preview_keyframe_moves_with_value_delta(
            &mut layer,
            &row,
            0,
            &baseline,
            &[0, 10],
            5,
            2.0,
        ));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.keyframes()[0].frame, 5);
        assert_eq!(curve.keyframes()[0].value, 2.0);
        assert_eq!(curve.keyframes()[1].frame, 15);
        assert_eq!(curve.keyframes()[1].value, 3.0);
    }

    #[test]
    fn tangent_preview_restarts_when_coupling_changes() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        let baseline = {
            let channels = row_channels(&layer, &row).unwrap();
            let ChannelSource::Keyframes(curve) = &channels[0].source else {
                panic!("expected keyframes");
            };
            curve.clone()
        };

        assert!(preview_keyframe_tangent(
            &mut layer,
            &row,
            0,
            &baseline,
            KeyframeTangentEdit {
                frame: 0,
                handle: TangentHandle::Out,
                tangent: Vec2(4.0, 2.0),
                coupling: TangentCoupling::Symmetric,
            },
        ));
        assert!(preview_keyframe_tangent(
            &mut layer,
            &row,
            0,
            &baseline,
            KeyframeTangentEdit {
                frame: 0,
                handle: TangentHandle::Out,
                tangent: Vec2(5.0, 3.0),
                coupling: TangentCoupling::Separated,
            },
        ));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        let key = curve.keyframes()[0];
        assert_eq!(key.tangent_out, Vec2(5.0, 3.0));
        assert_eq!(key.tangent_in, baseline.keyframes()[0].tangent_in);
    }

    #[test]
    fn tangent_delta_preview_updates_every_applicable_selected_key() {
        let mut layer = test_layer();
        let row = PropertyRowId::Shell(PropertyGroup::Rotation);
        let mut baseline = KeyframeCurve::new();
        baseline.insert_keyframe(
            ravel_core::animation::curve::Keyframe::new(0, 0.0, Interpolation::Bezier)
                .with_tangents(Vec2(0.0, 0.0), Vec2(3.0, 1.0)),
        );
        baseline.insert_keyframe(
            ravel_core::animation::curve::Keyframe::new(10, 1.0, Interpolation::Bezier)
                .with_tangents(Vec2(-3.0, -1.0), Vec2(3.0, 2.0)),
        );
        baseline.insert(20, 2.0, Interpolation::Linear);
        layer.transform.rotation = AnimationChannel::keyframes(baseline.clone());

        assert!(preview_keyframe_tangents_with_delta(
            &mut layer,
            &row,
            0,
            &baseline,
            KeyframeTangentDeltaEdit {
                frames: &[0, 10, 20],
                handle: TangentHandle::Out,
                delta: Vec2(1.0, 1.0),
                coupling: TangentCoupling::Separated,
            },
        ));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.keyframes()[0].tangent_out, Vec2(4.0, 2.0));
        assert_eq!(curve.keyframes()[1].tangent_out, Vec2(4.0, 3.0));
        assert_eq!(curve.keyframes()[2].tangent_out, Vec2(0.0, 0.0));
    }

    #[test]
    fn symmetric_tangent_drag_clamps_each_handle_to_its_interval() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Bezier);
        curve.insert(10, 1.0, Interpolation::Bezier);
        curve.insert(30, 2.0, Interpolation::Bezier);

        assert!(set_curve_tangent(
            &mut curve,
            10,
            TangentHandle::Out,
            Vec2(50.0, 3.0),
            TangentCoupling::Symmetric,
        ));

        let key = &curve.keyframes()[1];
        assert_eq!(key.tangent_out, Vec2(20.0, 3.0));
        assert_eq!(key.tangent_in, Vec2(-10.0, -1.5));
        assert_eq!(
            key.tangent_out.1 / key.tangent_out.0,
            key.tangent_in.1 / key.tangent_in.0,
            "the shorter mirrored handle remains collinear"
        );
    }

    #[test]
    fn separated_tangent_drag_preserves_the_opposite_handle() {
        let mut curve = KeyframeCurve::new();
        curve.insert_keyframe(
            ravel_core::animation::curve::Keyframe::new(10, 1.0, Interpolation::Bezier)
                .with_tangents(Vec2(-4.0, -2.0), Vec2(3.0, 1.0)),
        );
        curve.insert(20, 2.0, Interpolation::Bezier);

        assert!(set_curve_tangent(
            &mut curve,
            10,
            TangentHandle::Out,
            Vec2(-5.0, 6.0),
            TangentCoupling::Separated,
        ));

        let key = &curve.keyframes()[0];
        assert_eq!(key.tangent_out, Vec2(0.0, 6.0));
        assert_eq!(key.tangent_in, Vec2(-4.0, -2.0));
    }

    #[test]
    fn endpoint_tangents_only_use_the_existing_adjacent_interval() {
        let mut curve = KeyframeCurve::new();
        curve.insert(10, 1.0, Interpolation::Bezier);
        curve.insert(20, 2.0, Interpolation::Bezier);

        assert!(set_curve_tangent(
            &mut curve,
            10,
            TangentHandle::Out,
            Vec2(30.0, 6.0),
            TangentCoupling::Symmetric,
        ));
        assert_eq!(curve.keyframes()[0].tangent_out, Vec2(10.0, 6.0));
        assert_eq!(curve.keyframes()[0].tangent_in, Vec2(-10.0, -6.0));

        assert!(set_curve_tangent(
            &mut curve,
            20,
            TangentHandle::In,
            Vec2(-30.0, -9.0),
            TangentCoupling::Symmetric,
        ));
        assert_eq!(curve.keyframes()[1].tangent_in, Vec2(-10.0, -9.0));
        assert_eq!(curve.keyframes()[1].tangent_out, Vec2(10.0, 9.0));
    }

    #[test]
    fn interpolation_update_preserves_value_and_tangents() {
        let mut curve = KeyframeCurve::new();
        curve.insert_keyframe(
            ravel_core::animation::curve::Keyframe::new(10, 7.0, Interpolation::Linear)
                .with_tangents(Vec2(-2.0, -3.0), Vec2(4.0, 5.0)),
        );

        assert!(set_curve_interpolation(&mut curve, 10, Interpolation::Step));
        let key = &curve.keyframes()[0];
        assert_eq!(key.value, 7.0);
        assert_eq!(key.interpolation, Interpolation::Step);
        assert_eq!(key.tangent_in, Vec2(-2.0, -3.0));
        assert_eq!(key.tangent_out, Vec2(4.0, 5.0));
        assert!(!set_curve_interpolation(
            &mut curve,
            99,
            Interpolation::Bezier
        ));
    }

    #[test]
    fn bezier_conversion_seeds_visible_handles_without_bending_the_segment() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 10.0, Interpolation::Linear);
        curve.insert(12, 22.0, Interpolation::Linear);

        assert!(set_curve_interpolation(
            &mut curve,
            0,
            Interpolation::Bezier
        ));
        assert_eq!(curve.keyframes()[0].tangent_out, Vec2(4.0, 4.0));
        assert_eq!(curve.keyframes()[1].tangent_in, Vec2(-4.0, -4.0));
        for frame in 0..=12 {
            assert!((curve.sample(frame as f64) - (10.0 + frame as f32)).abs() < 1.0e-4);
        }
    }

    #[test]
    fn layer_tangent_and_interpolation_updates_rebuild_network_channel() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };

        assert!(set_keyframe_tangent(
            &mut layer,
            &row,
            0,
            0,
            TangentHandle::Out,
            Vec2(30.0, 2.0),
            TangentCoupling::Separated,
        ));
        assert!(set_keyframe_interpolation(
            &mut layer,
            &row,
            0,
            0,
            Interpolation::Bezier,
        ));

        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.keyframes()[0].tangent_out, Vec2(10.0, 2.0));
        assert_eq!(curve.keyframes()[0].interpolation, Interpolation::Bezier);
    }

    #[test]
    fn moving_neighbor_does_not_rewrite_saved_tangent_values() {
        let mut curve = KeyframeCurve::new();
        curve.insert_keyframe(
            ravel_core::animation::curve::Keyframe::new(0, 0.0, Interpolation::Bezier)
                .with_tangents(Vec2(-8.0, -1.0), Vec2(8.0, 1.0)),
        );
        curve.insert(10, 1.0, Interpolation::Bezier);

        assert!(curve.move_keyframe(10, 4));
        assert_eq!(
            curve.keyframes()[0].tangent_out,
            Vec2(8.0, 1.0),
            "the evaluator clamps x while the saved handle remains non-destructive"
        );
        assert!(curve.move_keyframe(4, 10));
        assert_eq!(curve.keyframes()[0].tangent_out, Vec2(8.0, 1.0));
    }

    // ----- subnets (HIGH-27 regression guards) ------------------------------

    /// [`test_layer`] with its `blur` collapsed two subnets deep, the way
    /// Collapse to Subnet run twice leaves it. The inner In of each subnet
    /// carries a keyframed `amount` that its owner promotes — the shadowed
    /// case the enumeration has to leave out.
    fn layer_with_nested_subnets() -> Layer {
        let inner_in = |id: NodeId| {
            Node::new(id, net::NET_IN_TYPE_KEY)
                .with_output(net::PORT_TIME, DataTypeId::SCALAR)
                .with_output("amount", DataTypeId::SCALAR)
                .with_param(
                    "amount",
                    ParameterValue::Channel(AnimationChannel::keyframes(curve_0_to_10())),
                )
        };
        let blur = Node::new(NodeId::new(20), "blur").with_param(
            "radius",
            ParameterValue::Channel(AnimationChannel::keyframes(curve_0_to_10())),
        );
        // Deepest network: net.in / net.out plus the blur.
        let deep = Graph::new()
            .add_node(inner_in(NodeId::new(30)))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(31), net::NET_OUT_TYPE_KEY)
                    .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            .add_node(blur)
            .unwrap();
        let mut deep_subnet = Node::new(NodeId::new(21), net::SUBNET_TYPE_KEY);
        deep_subnet.metadata.label = Some("Inner".into());
        net::adopt_subnet_inner(&mut deep_subnet, deep);

        let middle = Graph::new()
            .add_node(inner_in(NodeId::new(40)))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(41), net::NET_OUT_TYPE_KEY)
                    .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            .add_node(deep_subnet)
            .unwrap();
        let mut outer_subnet = Node::new(NodeId::new(11), net::SUBNET_TYPE_KEY);
        outer_subnet.metadata.label = Some("Outer".into());
        net::adopt_subnet_inner(&mut outer_subnet, middle);

        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_TIME, DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(outer_subnet)
            .unwrap();
        Layer::new(LayerId::new(1), "L", network).with_time(10, 5, 300)
    }

    /// A keyframed parameter that a Collapse to Subnet moved out of the top
    /// level still has a row, at any depth, named by the subnets it sits in.
    #[test]
    fn rows_reach_keyframes_inside_nested_subnets() {
        let rows = property_rows(&layer_with_nested_subnets());
        let network: Vec<(&PropertyRowId, Option<&str>)> = rows
            .iter()
            .filter(|row| matches!(row.id, PropertyRowId::Network { .. }))
            .map(|row| (&row.id, row.label.as_deref()))
            .collect();

        let blur = network
            .iter()
            .find(|(id, _)| {
                matches!(id, PropertyRowId::Network { node, key }
                    if *node == NodeId::new(20) && key == "radius")
            })
            .expect("the collapsed blur's keyframes are visible two levels down");
        assert_eq!(blur.1, Some("Outer / Inner / blur · radius"));

        // The promoted parameters of both subnet nodes are the live controls
        // and keep their rows; the inner In parameters they shadow do not get
        // one, because editing those would animate nothing.
        assert!(
            network.iter().any(|(id, _)| matches!(id,
                PropertyRowId::Network { node, key }
                    if *node == NodeId::new(11) && key == "amount")),
            "the outer subnet's promoted parameter is a row"
        );
        assert!(
            !network.iter().any(|(id, _)| matches!(id,
                PropertyRowId::Network { node, .. } if *node == NodeId::new(40))),
            "the inner In it shadows is not"
        );
        assert!(
            !network.iter().any(|(id, _)| matches!(id,
                PropertyRowId::Network { node, .. } if *node == NodeId::new(30))),
            "and neither is the one a level deeper"
        );
    }

    /// The row is editable: a key inserted through it reaches the parameter in
    /// the subnet's own graph, and removing it again reverts the channel.
    #[test]
    fn keyframes_inside_a_subnet_are_editable_through_their_row() {
        let mut layer = layer_with_nested_subnets();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        assert!(row_channels(&layer, &row).is_some(), "the row resolves");

        assert!(insert_keyframe(&mut layer, &row, 0, 5));
        assert!(has_keyframe_at(&layer, &row, 0, 5));
        let stored = layer
            .network
            .find_nested_node(NodeId::new(20))
            .expect("the blur is still inside the nested subnet")
            .parameters
            .iter()
            .find(|p| p.key == "radius")
            .map(|p| p.value.clone());
        assert!(
            matches!(stored, Some(ParameterValue::Channel(ref ch))
                if matches!(&ch.source, ChannelSource::Keyframes(curve)
                    if curve.keyframes().iter().any(|k| k.frame == 5))),
            "the edit landed in the inner graph, not on a discarded copy"
        );

        assert!(move_keyframe(&mut layer, &row, 0, 5, 6));
        assert!(has_keyframe_at(&layer, &row, 0, 6));
        assert!(remove_keyframe(&mut layer, &row, 0, 6));
        assert!(!has_keyframe_at(&layer, &row, 0, 6));

        // The layer's own top-level structure is untouched by all of it.
        assert!(layer.network.node(NodeId::new(11)).is_some());
        assert!(layer.network.node(NodeId::new(20)).is_none());
    }

    // ----- discrete keyframes: `IntChannel` and `StringSteps` rows ----------

    fn steps_ten_twenty() -> StepCurve<String> {
        let mut steps = StepCurve::new("fallback".to_string());
        steps.insert(10, "ten".to_string());
        steps.insert(20, "twenty".to_string());
        steps
    }

    /// `test_layer` plus a node carrying an animatable int (`count`) and an
    /// animatable string (`text`).
    fn discrete_layer() -> Layer {
        let node = Node::new(NodeId::new(30), "repeat")
            .with_param(
                "count",
                ParameterValue::IntChannel(AnimationChannel::keyframes(curve_0_to_10())),
            )
            .with_param("text", ParameterValue::StringSteps(steps_ten_twenty()));
        let mut layer = test_layer();
        layer.network = layer.network.clone().add_node(node).unwrap();
        layer
    }

    fn int_row() -> PropertyRowId {
        PropertyRowId::Network {
            node: NodeId::new(30),
            key: "count".into(),
        }
    }

    fn string_row() -> PropertyRowId {
        PropertyRowId::Network {
            node: NodeId::new(30),
            key: "text".into(),
        }
    }

    /// Both discrete kinds earn a Timeline row, each with one lane named
    /// "the value" — the same shape a single-component float parameter has.
    #[test]
    fn int_and_string_parameters_get_property_rows() {
        let rows = property_rows(&discrete_layer());
        let int = rows
            .iter()
            .find(|row| row.id == int_row())
            .expect("int row");
        let string = rows
            .iter()
            .find(|row| row.id == string_row())
            .expect("string row");
        assert_eq!(int.channel_names, vec![CHANNEL_VALUE]);
        assert_eq!(string.channel_names, vec![CHANNEL_VALUE]);
        assert_eq!(int.label.as_deref(), Some("repeat · count"));
        assert_eq!(string.label.as_deref(), Some("repeat · text"));
    }

    /// A step curve with no keys is a parameter nobody keyed, so it stays out
    /// of the tree exactly as a constant channel does.
    #[test]
    fn an_empty_step_curve_has_no_row() {
        let node = Node::new(NodeId::new(30), "repeat").with_param(
            "text",
            ParameterValue::StringSteps(StepCurve::new("a".to_string())),
        );
        let mut layer = test_layer();
        layer.network = layer.network.clone().add_node(node).unwrap();
        assert!(
            !property_rows(&layer)
                .iter()
                .any(|row| row.id == string_row())
        );
    }

    /// The value kind is what closes interpolation and tangent editing on a
    /// step row, and what tells the curve editor to draw an int as a
    /// staircase. A shell row is always float.
    #[test]
    fn row_value_kind_separates_float_int_and_steps() {
        let layer = discrete_layer();
        assert_eq!(
            row_value_kind(&layer, &PropertyRowId::Shell(PropertyGroup::Position)),
            RowValueKind::Float
        );
        assert_eq!(
            row_value_kind(
                &layer,
                &PropertyRowId::Network {
                    node: NodeId::new(20),
                    key: "radius".into()
                }
            ),
            RowValueKind::Float
        );
        assert_eq!(row_value_kind(&layer, &int_row()), RowValueKind::Integer);
        assert_eq!(row_value_kind(&layer, &string_row()), RowValueKind::Steps);

        assert!(!RowValueKind::Float.is_stepped());
        assert!(!RowValueKind::Integer.is_stepped());
        assert!(RowValueKind::Steps.is_stepped());
        assert!(RowValueKind::Integer.is_integral());
        assert!(!RowValueKind::Steps.is_integral());
        assert!(!RowValueKind::Float.is_integral());
    }

    /// A step row has one lane even though it has no `AnimationChannel`:
    /// counting channels would give it zero and desynchronize the painter
    /// from the hit test below it.
    #[test]
    fn a_step_row_has_one_lane_and_its_keys_enumerate() {
        let layer = discrete_layer();
        assert_eq!(row_component_count(&layer, &string_row()), 1);
        assert!(row_channels(&layer, &string_row()).is_none());
        assert_eq!(row_key_frames(&layer, &string_row(), 0), vec![10, 20]);
        assert!(row_key_frames(&layer, &string_row(), 1).is_empty());
        assert!(has_keyframe_at(&layer, &string_row(), 0, 10));
        assert!(!has_keyframe_at(&layer, &string_row(), 0, 11));
        // The int row keeps the float channel it is made of.
        assert_eq!(row_component_count(&layer, &int_row()), 1);
        assert_eq!(row_key_frames(&layer, &int_row(), 0), vec![0, 10]);
    }

    /// Insert re-keys the string the frame already holds, move preserves the
    /// value, and remove takes the key away.
    #[test]
    fn step_keys_can_be_added_moved_and_removed() {
        let mut layer = discrete_layer();
        let row = string_row();

        // Frame 15 holds "ten"; keying it there pins that value.
        assert!(insert_keyframe(&mut layer, &row, 0, 15));
        assert_eq!(row_key_frames(&layer, &row, 0), vec![10, 15, 20]);

        assert!(move_keyframe(&mut layer, &row, 0, 15, 17));
        assert_eq!(row_key_frames(&layer, &row, 0), vec![10, 17, 20]);
        let ParameterValue::StringSteps(steps) = row_parameter_value(&layer, &row).unwrap() else {
            panic!("still a step curve");
        };
        assert_eq!(steps.sample(17.0), "ten", "the moved key kept its value");

        assert!(remove_keyframe(&mut layer, &row, 0, 17));
        assert_eq!(row_key_frames(&layer, &row, 0), vec![10, 20]);
        assert!(!remove_keyframe(&mut layer, &row, 0, 17), "already gone");
        // Lane 1 does not exist on a step row.
        assert!(!insert_keyframe(&mut layer, &row, 1, 5));
    }

    /// Removing the last key returns the parameter to a plain `String`
    /// holding the curve's **default** — the constant it had before it was
    /// keyed — and the row drops out of the tree, mirroring the float rule.
    #[test]
    fn emptying_a_step_curve_restores_the_constant_string() {
        let mut layer = discrete_layer();
        let row = string_row();
        assert!(remove_keyframe(&mut layer, &row, 0, 10));
        assert!(remove_keyframe(&mut layer, &row, 0, 20));
        assert_eq!(
            row_parameter_value(&layer, &row),
            Some(&ParameterValue::String("fallback".to_string()))
        );
        assert!(!property_rows(&layer).iter().any(|r| r.id == row));
    }

    /// The drag preview rebuilds from the pre-gesture snapshot, so a
    /// transient pass over an occupied frame does not merge two keys.
    #[test]
    fn a_step_drag_preview_rebuilds_from_its_baseline() {
        let mut layer = discrete_layer();
        let row = string_row();
        let baseline = row_keys(&layer, &row, 0).expect("snapshot");
        assert!(baseline.curve().is_none(), "a step row has no float curve");

        // Drag the key at 10 right over the key at 20 …
        assert!(preview_row_key_moves(
            &mut layer,
            &row,
            0,
            &baseline,
            &[10],
            10
        ));
        assert_eq!(row_key_frames(&layer, &row, 0), vec![20]);
        // … and back off it: the overwritten key returns.
        assert!(preview_row_key_moves(
            &mut layer,
            &row,
            0,
            &baseline,
            &[10],
            5
        ));
        assert_eq!(row_key_frames(&layer, &row, 0), vec![15, 20]);
        let ParameterValue::StringSteps(steps) = row_parameter_value(&layer, &row).unwrap() else {
            panic!("still a step curve");
        };
        assert_eq!(steps.sample(15.0), "ten");
        assert_eq!(steps.sample(20.0), "twenty");
    }

    /// A float row's snapshot still carries its curve, so the value-axis and
    /// tangent gestures the curve editor drives keep working.
    #[test]
    fn a_float_row_snapshot_carries_its_curve() {
        let layer = discrete_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        let baseline = row_keys(&layer, &row, 0).expect("snapshot");
        assert!(baseline.curve().is_some());
        // The int row is a float row for every editing purpose.
        assert!(row_keys(&layer, &int_row(), 0).unwrap().curve().is_some());
    }

    /// An identifier parameter must never be animated
    /// (`is_identifier_parameter`): the Properties toggle refuses it, and the
    /// Timeline refuses to show a row for one even if a document carries it
    /// anyway, so no gesture here can grow it further.
    #[test]
    fn identifier_parameters_get_no_timeline_row() {
        let layer_ref = Node::new(NodeId::new(40), "layer.ref")
            .with_param(
                "layer",
                ParameterValue::IntChannel(AnimationChannel::keyframes(curve_0_to_10())),
            )
            .with_param("port", ParameterValue::StringSteps(steps_ten_twenty()));
        let precomp = Node::new(NodeId::new(41), "precomp").with_param(
            "comp_id",
            ParameterValue::IntChannel(AnimationChannel::keyframes(curve_0_to_10())),
        );
        let mut layer = test_layer();
        layer.network = layer
            .network
            .clone()
            .add_node(layer_ref)
            .unwrap()
            .add_node(precomp)
            .unwrap();

        let rows = property_rows(&layer);
        assert!(
            !rows.iter().any(|row| matches!(
                &row.id,
                PropertyRowId::Network { key, .. } if key == "layer" || key == "comp_id"
            )),
            "the reference parameters are not animatable, so they get no row"
        );
        // A non-identifier parameter on the same node still does.
        assert!(rows.iter().any(|row| row.id
            == PropertyRowId::Network {
                node: NodeId::new(40),
                key: "port".into()
            }));
    }
}

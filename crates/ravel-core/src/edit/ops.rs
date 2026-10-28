// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::document::CompositionSettings;
use super::document::NetworkPath;
use crate::composition::{BlendMode, ShellProperty};
use crate::id::{CompId, EdgeId, LayerId, NodeId};
use crate::param_curve::CurveParam;
use crate::param_ramp::RampParam;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A composition named by id or by an alias an earlier operation of the same
/// batch defined with `as`. In JSON a number is an id and a string an alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompRef {
    Id(CompId),
    Alias(String),
}

/// A layer named by id or by alias; see [`CompRef`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LayerRef {
    Id(LayerId),
    Alias(String),
}

/// A node named by id or by alias; see [`CompRef`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NodeRef {
    Id(NodeId),
    Alias(String),
}

/// The network an operation edits, as a caller writes it: the
/// [`NetworkPath`] fields, each of which may be an alias an earlier operation
/// of the batch defined. A path written with numbers reads as before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRef {
    pub comp: CompRef,
    pub layer: LayerRef,
    /// Subnet nodes entered from the layer network, outermost first.
    #[serde(default)]
    pub subnets: Vec<NodeRef>,
}

impl From<NetworkPath> for NetworkRef {
    fn from(path: NetworkPath) -> Self {
        Self {
            comp: CompRef::Id(path.comp),
            layer: LayerRef::Id(path.layer),
            subnets: path.subnets.into_iter().map(NodeRef::Id).collect(),
        }
    }
}

impl From<&NetworkPath> for NetworkRef {
    fn from(path: &NetworkPath) -> Self {
        path.clone().into()
    }
}

/// A port named by its zero-based number or by its name. In JSON a number is
/// an index and a string a name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PortRef {
    Index(u32),
    Name(String),
}

/// A parameter value as a caller writes it. Adjacently tagged in JSON
/// (`{"type": "float", "value": 0.5}`) because a bare number cannot tell an
/// int from a float.
///
/// It is the value half of a property edit in the UI (`PropertyValue` there is
/// this type), not the stored [`ParameterValue`](crate::graph::ParameterValue):
/// it carries no animation, so what a write does to an animated parameter is
/// the write rules' decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ParamInput {
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    Color {
        r: f32,
        g: f32,
        b: f32,
        a: f32,
    },
    /// Components of a vector parameter, leading ones first.
    Vector(Vec<f32>),
    /// A whole edited curve. Curve edits replace the control-point set
    /// rather than a scalar, so the gesture granularity is the same as a
    /// scrub's: live edits apply uncommitted and the gesture's last value
    /// records one undo step.
    Curve(CurveParam),
    /// A whole edited ramp, on the same terms as [`ParamInput::Curve`]: a stop
    /// move, an add, a remove, a colour change or an interpolation switch all
    /// replace the stop set rather than one scalar.
    Ramp(RampParam),
}

/// How a key's outgoing segment is interpolated. A bezier key gets the
/// editor's default handles (one third of the way to the next key), not
/// caller-chosen ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyInterpolation {
    Bezier,
    Linear,
    Step,
}

impl From<KeyInterpolation> for crate::animation::interpolation::Interpolation {
    fn from(value: KeyInterpolation) -> Self {
        match value {
            KeyInterpolation::Bezier => Self::Bezier,
            KeyInterpolation::Linear => Self::Linear,
            KeyInterpolation::Step => Self::Step,
        }
    }
}

/// One key of [`EditOp::SetKeyframes`]. `frame` is a **composition** frame;
/// the edit stores the layer-local one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyInput {
    pub frame: u64,
    pub value: f32,
    pub interpolation: KeyInterpolation,
}

/// What an animation operation addresses, in the two kinds of row the
/// Timeline has (`PropertyRowId::Network` / `Shell`). In JSON the fields sit
/// beside the operation's own: `path` + `node` + `key` name a node parameter,
/// `comp` + `layer` + `property` a shell property.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChannelTarget {
    Parameter {
        path: NetworkRef,
        node: NodeRef,
        key: String,
    },
    Shell {
        comp: CompRef,
        layer: LayerRef,
        property: ShellProperty,
    },
}

/// Distinguishes an absent field (leave alone) from `null` (clear).
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// One end of a connection: a node and one of its ports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub node: NodeRef,
    pub port: PortRef,
}

/// One typed edit of a [`Document`](crate::composition::Document).
///
/// The set is closed on purpose: `match` exhaustiveness makes a new operation
/// without an implementation a compile error. The operations that create
/// something take an optional `as` alias (`alias` in Rust) that later
/// operations of the same batch can use as a reference; aliases are not kept
/// beyond the batch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    AddComposition {
        settings: CompositionSettings,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    RemoveComposition {
        comp: CompRef,
    },
    /// Stack a layer built from the builtin template `template` on top of
    /// `comp`.
    AddLayer {
        comp: CompRef,
        template: String,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    /// Remove layers. A locked layer is an error, not a skip.
    RemoveLayer {
        comp: CompRef,
        layers: Vec<LayerRef>,
    },
    /// Create a node of the registered type `type_key` in the network `path`
    /// addresses, above everything already there.
    AddNode {
        path: NetworkRef,
        type_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<(f32, f32)>,
        /// Parameters set after creation, with the rules of
        /// [`EditOp::SetParameter`] (so an unknown key refuses the whole node).
        /// Applied in key order.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        params: BTreeMap<String, ParamInput>,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    /// Remove a node and the edges touching it. A network's own `net.in` /
    /// `net.out` cannot be removed.
    RemoveNode {
        path: NetworkRef,
        node: NodeRef,
    },
    /// Connect an output port to an input port. An edge already on that input
    /// is replaced, and a variadic group keeps exactly one free slot.
    Connect {
        path: NetworkRef,
        from: Endpoint,
        to: Endpoint,
    },
    /// Remove one edge; its target's variadic group is compacted.
    Disconnect {
        path: NetworkRef,
        edge: EdgeId,
    },
    /// Write one parameter. A number is clamped to the registry range, an
    /// animated parameter gets a key at `EditContext::local_frame` (and is an
    /// error without one), and the values the write decides — `attribute.set`'s
    /// `value`, `layer.ref`'s output port — follow in the same step.
    SetParameter {
        path: NetworkRef,
        node: NodeRef,
        key: String,
        value: ParamInput,
    },
    /// Replace one channel of `target` by a curve through `keys` (frames in
    /// the composition's timeline). `component` counts from 0: x/y of a
    /// position, r/g/b/a of a colour (linear light). A channel driven by an
    /// expression, a node output or a blend is refused (`driven-parameter`).
    SetKeyframes {
        #[serde(flatten)]
        target: ChannelTarget,
        #[serde(default)]
        component: u32,
        keys: Vec<KeyInput>,
    },
    /// Drop the animation (keys or expression) of one channel, leaving the
    /// constant `value`.
    ClearAnimation {
        #[serde(flatten)]
        target: ChannelTarget,
        #[serde(default)]
        component: u32,
        value: f32,
    },
    /// Set shell properties of a layer; absent fields are left alone. A
    /// transform or opacity constant is refused on an animated channel
    /// (`animated-parameter`) or a driven one (`driven-parameter`);
    /// `parent: null` clears the parent.
    SetLayer {
        comp: CompRef,
        layer: LayerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_frame: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        in_frame: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        out_frame: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor_point: Option<[f32; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<[f32; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scale: Option<[f32; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opacity: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blend_mode: Option<BlendMode>,
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        parent: Option<Option<LayerRef>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        solo: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        muted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locked: Option<bool>,
    },
}

impl EditOp {
    /// Whether the operation deletes something. Only a classification: asking
    /// the user is the caller's job. `ClearAnimation` is not one: undo brings
    /// the keys back.
    pub fn is_destructive(&self) -> bool {
        match self {
            Self::AddComposition { .. }
            | Self::AddLayer { .. }
            | Self::AddNode { .. }
            | Self::Connect { .. }
            | Self::SetParameter { .. }
            | Self::SetKeyframes { .. }
            | Self::ClearAnimation { .. }
            | Self::SetLayer { .. } => false,
            Self::RemoveComposition { .. }
            | Self::RemoveLayer { .. }
            | Self::RemoveNode { .. }
            | Self::Disconnect { .. } => true,
        }
    }
}

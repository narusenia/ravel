// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node template registry for built-in and user-defined node types.

pub mod builtin;

use crate::composition::{Composition, Layer};
use crate::graph::{InputPort, Node, OutputPort, Parameter};
use crate::id::{LayerId, NodeId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::RangeInclusive;

/// Data-domain grouping of node templates, used by the add-node menu and
/// the node header tint. Categories follow the data a node deals with
/// (its port types), not its function — a geometry transform belongs to
/// `Geometry`, an image transform to `Image` — so the category color can
/// reuse the port palette without contradictions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeCategory {
    /// Geometry sources and operators (shapes, scatter, attributes).
    Geometry,
    /// 3D scene assembly: objects, transform hierarchies, and cameras.
    Scene,
    /// Field sources and operators.
    Field,
    /// Frame-buffer sources, compositing, and effects.
    Image,
    /// Color values and color processing.
    Color,
    /// Time-domain utilities (currently unpopulated).
    Time,
    /// Scalar math, values, and structural helpers (subnet, layer ref).
    Utility,
}

/// Editing range metadata for a numeric parameter.
///
/// `hard` is the true clamp boundary — a value never leaves it. `ui` is the
/// comfortable editing span widgets present by default (slider bounds, scrub
/// sensitivity); it must be contained in `hard`. Int parameters share the
/// same f32-based ranges and cast at the edges.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamRange {
    pub hard: RangeInclusive<f32>,
    pub ui: RangeInclusive<f32>,
}

impl ParamRange {
    pub fn new(hard: RangeInclusive<f32>, ui: RangeInclusive<f32>) -> Self {
        debug_assert!(
            hard.start() <= ui.start() && ui.end() <= hard.end(),
            "ui range {ui:?} must be contained in hard range {hard:?}"
        );
        Self { hard, ui }
    }

    /// Clamps a value to the hard boundary.
    pub fn clamp(&self, value: f32) -> f32 {
        value.clamp(*self.hard.start(), *self.hard.end())
    }
}

/// What a parameter *means* geometrically, so direct manipulation can put a
/// handle on it instead of guessing from its name.
///
/// The declaration lives on the template because the meaning belongs to the
/// node type, not to the Viewer: a manipulator driven by names would have to
/// carry a table of every built-in's spelling, and adding a node would mean
/// editing the Viewer. Roles apply to vector parameters (`Channel2` /
/// `Channel3`); the canvas is two-dimensional, so a `Channel3` is driven by
/// its X and Y and keeps its Z.
///
/// [`Size`] is measured from the node's [`Position`] parameter — the first one
/// it declares — or from the local origin when it declares none.
///
/// Only the roles the manipulator actually draws a handle for live here. A
/// direction (which needs a display length) and an angle (which needs a pivot
/// convention) arrive with the unit that draws them: a declared role that
/// silently does nothing is the trap `style-attributes-plan.md` declined for
/// `stroke_align`.
///
/// [`Position`]: ParamRole::Position
/// [`Size`]: ParamRole::Size
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamRole {
    /// A point in the node's local space (`shape.rect`'s `center`).
    Position,
    /// An offset from the position: a radius, a half extent.
    Size,
}

/// Declares that a four-component parameter is a **colour**, so the editors
/// draw it as one.
///
/// Deliberately **not** a [`ParamRole`] arm. `ParamRole` is the set of
/// geometric meanings the Viewer's manipulator draws a handle for, and its
/// own doc comment refuses roles that draw nothing; "this is a colour" draws
/// no handle and is not a position in the node's space. The two answer
/// different questions — where is it, versus how is it read — so they are
/// two declarations.
///
/// **What this decides is how the value is drawn, and nothing else**: the
/// Properties field kind (a colour swatch instead of four scrubs) and the
/// Timeline's component names (`R`/`G`/`B`/`A` instead of `X`/`Y`/`Z`/`W`).
/// The *exposed* parameter declaration type still comes from
/// `exposed::apply::seed_value` alone, which is the single owner of the
/// `ParameterValue` → external-contract mapping
/// (`docs/dev/add-node.md`). Adding a second table there would let the panel
/// declare something `apply` cannot write back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorParam {
    /// The parameter is always a colour.
    Always,
    /// The parameter is a colour only while the node's `key` parameter reads
    /// `value`.
    ///
    /// `attribute.set` needs this: its `value` is the same `Channel4` for
    /// `type = "color"` and for `type = "vec4"`, and only the first is a
    /// colour (`Graph::port_accepted_types` states the same split).
    When { key: String, value: String },
}

impl ColorParam {
    /// Whether this declaration holds for `node` right now.
    fn holds_for(&self, node: &Node) -> bool {
        match self {
            Self::Always => true,
            Self::When { key, value } => node
                .parameters
                .iter()
                .find(|p| &p.key == key)
                .and_then(|p| p.value.as_str())
                .is_some_and(|current| current == value),
        }
    }
}

/// Whether `key` on `node` is drawn as a colour rather than as a plain
/// 4-component vector.
///
/// Only a four-component parameter can be one: `Channel2` / `Channel3` are
/// vectors whatever the template says, and the registry has no three-channel
/// colour. Everything else is `false`, so a caller may ask about any key.
///
/// Three sources, in order:
///
/// 1. **A custom-port node** — a network-interface In node, or a subnet node
///    carrying the promoted parameters of one. Their parameters have no
///    template to declare them; the custom port the user picked is the
///    declaration, and [`crate::network::CustomPortType`] offers `Color` and
///    no `Vec4`, so every four-component custom parameter is a colour.
/// 2. **The template's** [`NodeTemplate::color_param`] declaration.
/// 3. Nothing: an undeclared `Channel4` is a vector (`MED-APP-19`). A new
///    node type whose colour is undeclared therefore renders as four scrubs
///    — `builtin::tests` enumerates every built-in `Channel4` to catch that.
pub fn is_color_parameter(registry: &NodeRegistry, node: &Node, key: &str) -> bool {
    let Some(param) = node.parameters.iter().find(|p| p.key == key) else {
        return false;
    };
    if !matches!(param.value, crate::graph::ParameterValue::Channel4(_)) {
        return false;
    }
    if node.type_key == crate::network::NET_IN_TYPE_KEY || node.subnet.is_some() {
        return true;
    }
    registry
        .get(&node.type_key)
        .and_then(|template| template.color_param(key))
        .is_some_and(|declaration| declaration.holds_for(node))
}

/// One entry of a closed option set: the value that is **stored** and the text
/// that is **shown**.
///
/// Two fields rather than one string because the two are not always the same
/// thing. A fixed set names its own values (`over`, `Normal`), so value and
/// label coincide; a contextual one addresses something the document holds,
/// and a layer is addressed by its [`LayerId`] while a user reads its name.
/// Packing both into one string — `"3: Background"`, parsed back apart at the
/// edit — is what this type replaces: the format was the only record of which
/// half was data, and every reader had to know it.
///
/// A label equal to its value is what the display boundary reads as "this is
/// a fixed option", which is how a state word emitted as a locale key still
/// gets translated while a layer name never does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamOption {
    /// What an edit writes.
    pub value: String,
    /// What the user reads. Equal to `value` for a fixed option.
    pub label: String,
}

impl ParamOption {
    /// An option the user reads as its own value — every [`ParamOptions::Fixed`]
    /// entry, plus a state word carried as a locale key.
    pub fn fixed(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
        }
    }

    /// An option whose display text differs from the value it stores.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// Where a string parameter's closed option set comes from.
///
/// [`Fixed`] is the whole of what the registry could declare before: a list
/// written into the template. [`Contextual`] declares only that the candidates
/// are decided by **where the node sits**, and names the kind of thing they
/// are; [`contextual_options`] resolves it against a composition.
///
/// A closed enum rather than a closure because [`NodeTemplate`] is data that is
/// cloned and compared, and because the resolution must stay in `ravel-core`:
/// a template carrying UI-supplied logic would put the panel in charge of what
/// a node type means.
///
/// [`Fixed`]: ParamOptions::Fixed
/// [`Contextual`]: ParamOptions::Contextual
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamOptions {
    /// Candidates written into the template.
    Fixed(Vec<String>),
    /// Candidates the document decides.
    Contextual(ContextualKind),
}

/// The kinds of contextual candidate the registry knows how to resolve.
///
/// Closed and small on purpose: each arm is an arm of [`contextual_options`],
/// so an arm that nothing resolves cannot be declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextualKind {
    /// The other layers of the composition the node's network belongs to.
    SiblingLayer,
    /// The output ports of the layer the node's `layer` parameter names —
    /// which are the **input** ports of that layer's `net.out` node
    /// (REQ-LAYER-002/003), the same ports `layer.ref` reads at evaluation
    /// time. Decided by the node's own parameter, so this is the kind that
    /// makes [`contextual_options`] need the node.
    LayerOutputPort,
}

/// One layer as an option: its [`LayerId`] is the value, `"{row}. {name}"`
/// the label, where `row` is the layer's **Timeline row number** — the only
/// number for a layer the user ever sees. A raw `LayerId` is an internal
/// counter, and a position inside a *filtered* candidate list would disagree
/// with the Timeline as soon as one candidate is excluded.
///
/// `index` is the layer's position in `comp.layers` and `total` that vector's
/// length, because the row number is **neither of them**: `comp.layers` is
/// bottom-most first (`Composition::move_layer` calls it the compositing
/// order) while the Timeline draws the last element in its first row
/// (`layer_blocks` walks `layers().rev()`). So row 1 is the topmost layer,
/// which is `comp.layers.len() - index`. Both halves of the conversion live
/// here, once: a caller that did its own arithmetic is a caller that can get
/// the direction wrong.
pub fn layer_param_option(index: usize, total: usize, layer: &Layer) -> ParamOption {
    ParamOption::new(
        layer.id.raw().to_string(),
        format!("{}. {}", total.saturating_sub(index), layer.name),
    )
}

/// The `net.out` node of the layer a `layer.ref`'s `layer` parameter names,
/// and `None` when the reference resolves to nothing: no target set, a layer
/// this composition does not hold, a target that does not stand still
/// ([`ParameterValue::identifier`] answers `Dynamic`), or a network with no
/// `net.out` node.
///
/// One place parses the stored decimal `LayerId` for both readers of the
/// reference — the port candidates and the output type that follows them —
/// so "unresolvable" means the same thing to each.
///
/// [`ParameterValue::identifier`]: crate::graph::ParameterValue::identifier
pub(crate) fn layer_ref_out_node<'a>(
    comp: &'a Composition,
    layer: &crate::graph::ParameterValue,
) -> Option<&'a std::sync::Arc<Node>> {
    let target = comp.get_layer(LayerId::new(layer.identifier().static_raw()?))?;
    crate::network::find_out_node(&target.network)
}

/// Resolve a [`ParamOptions::Contextual`] declaration against the composition
/// the node's network lives in.
///
/// `owner` is the layer that owns the network the node sits in, and `None`
/// when the node belongs to no layer. A node with no owning layer has no
/// siblings, so the candidate list is **empty** rather than "every layer":
/// offering the whole stack to a node whose own place in it is unknown would
/// offer a self-reference, which `validate_layer_ref_cycles` then rejects.
///
/// [`ContextualKind::SiblingLayer`] keeps the compositing order of
/// `comp.layers` and drops the owner — a layer is never its own sibling. The
/// labels are numbered by Timeline row, which runs the other way
/// ([`layer_param_option`]).
///
/// [`ContextualKind::LayerOutputPort`] reads `node`'s own `layer` parameter:
/// the candidates are the ports of the layer *that* names, so neither the
/// composition nor the owner decides them. A reference that resolves to
/// nothing offers nothing ([`layer_ref_out_node`]), which is also what keeps
/// the row from silently pointing somewhere else.
pub fn contextual_options(
    kind: ContextualKind,
    node: &Node,
    comp: &Composition,
    owner: Option<LayerId>,
) -> Vec<ParamOption> {
    match kind {
        ContextualKind::SiblingLayer => {
            // An owner the composition does not hold is "no owning layer",
            // not "every layer is a sibling": a stale target — a layer deleted
            // while its network was on screen — must offer nothing rather than
            // a stack the node is no longer part of.
            let Some(owner) = owner.filter(|id| comp.get_layer(*id).is_some()) else {
                return Vec::new();
            };
            let total = comp.layers.len();
            comp.layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| layer.id != owner)
                .map(|(index, layer)| layer_param_option(index, total, layer))
                .collect()
        }
        // The port names are the document's own text — a user named those
        // custom ports — but a port is read as itself, so value and label
        // coincide and `ParamOption::fixed` is what says so.
        ContextualKind::LayerOutputPort => node
            .parameters
            .iter()
            .find(|p| p.key == "layer")
            .and_then(|p| layer_ref_out_node(comp, &p.value))
            .map(|out| {
                out.inputs
                    .iter()
                    .map(|port| ParamOption::fixed(port.name.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[derive(Clone, Debug)]
pub struct NodeTemplate {
    pub type_key: String,
    pub label: String,
    pub category: NodeCategory,
    pub inputs: Vec<InputPort>,
    /// Base specification for a contiguous variadic group appended after all
    /// fixed inputs. A created node starts with one disconnected group slot.
    pub variadic_input_group: Option<InputPort>,
    pub outputs: Vec<OutputPort>,
    pub default_params: Vec<Parameter>,
    pub param_ranges: HashMap<String, ParamRange>,
    /// Closed option sets for string parameters (rendered as enum
    /// dropdowns instead of free-text fields), each either written into the
    /// template or delegated to the node's context ([`ParamOptions`]).
    pub param_options: HashMap<String, ParamOptions>,
    /// Geometric meanings the Viewer's manipulator reads.
    pub param_roles: HashMap<String, ParamRole>,
    /// Four-component parameters that are colours, which is what decides
    /// whether an editor draws a swatch or four numbered components
    /// ([`is_color_parameter`]).
    pub color_params: HashMap<String, ColorParam>,
    /// Display groups for this type's parameters: a group name (whose locale
    /// key is `node.<type_key>.group.<name>`) and the parameter keys it
    /// holds, in the order the Properties sections should appear.
    ///
    /// A `Vec` rather than the `HashMap` the metadata above uses because the
    /// order *is* part of the declaration here — it decides the section
    /// order, and a hash map has none. Parameters no group names stay
    /// together in one leading section, so a type that declares nothing
    /// looks exactly as it did
    /// (`docs/implementation/parameter-groups-plan.md`, PGRP-1).
    pub param_groups: Vec<(String, Vec<String>)>,
}

impl NodeTemplate {
    pub fn new(
        type_key: impl Into<String>,
        label: impl Into<String>,
        category: NodeCategory,
    ) -> Self {
        Self {
            type_key: type_key.into(),
            label: label.into(),
            category,
            inputs: Vec::new(),
            variadic_input_group: None,
            outputs: Vec::new(),
            default_params: Vec::new(),
            param_ranges: HashMap::new(),
            param_options: HashMap::new(),
            param_roles: HashMap::new(),
            color_params: HashMap::new(),
            param_groups: Vec::new(),
        }
    }

    pub fn with_input(mut self, port: InputPort) -> Self {
        self.inputs.push(port);
        self
    }

    /// Declares one variadic input group after the template's fixed inputs.
    /// `port` supplies the first slot's display name and accepted types.
    pub fn with_variadic_input_group(mut self, port: InputPort) -> Self {
        self.variadic_input_group = Some(port);
        self
    }

    pub fn with_output(mut self, port: OutputPort) -> Self {
        self.outputs.push(port);
        self
    }

    pub fn with_param(mut self, param: Parameter) -> Self {
        self.default_params.push(param);
        self
    }

    /// Attaches hard/UI editing ranges to a numeric parameter.
    pub fn with_param_range(
        mut self,
        key: impl Into<String>,
        hard: RangeInclusive<f32>,
        ui: RangeInclusive<f32>,
    ) -> Self {
        self.param_ranges
            .insert(key.into(), ParamRange::new(hard, ui));
        self
    }

    pub fn param_range(&self, key: &str) -> Option<&ParamRange> {
        self.param_ranges.get(key)
    }

    /// Declares the fixed closed option set of a string parameter.
    pub fn with_param_options<S: Into<String>>(
        mut self,
        key: impl Into<String>,
        options: impl IntoIterator<Item = S>,
    ) -> Self {
        self.param_options.insert(
            key.into(),
            ParamOptions::Fixed(options.into_iter().map(Into::into).collect()),
        );
        self
    }

    /// Declares that a string parameter's candidates come from the node's
    /// context rather than from this template ([`contextual_options`]).
    pub fn with_contextual_param_options(
        mut self,
        key: impl Into<String>,
        kind: ContextualKind,
    ) -> Self {
        self.param_options
            .insert(key.into(), ParamOptions::Contextual(kind));
        self
    }

    /// The **fixed** option values of `key`, and `None` for a contextual
    /// declaration: a caller that cannot supply a context cannot be answered,
    /// and answering it with an empty list would read as "no candidates".
    /// [`Self::param_option_source`] is what asks for the declaration itself.
    pub fn param_option_values(&self, key: &str) -> Option<&[String]> {
        match self.param_options.get(key)? {
            ParamOptions::Fixed(values) => Some(values.as_slice()),
            ParamOptions::Contextual(_) => None,
        }
    }

    /// The option-set declaration of `key`, fixed or contextual.
    pub fn param_option_source(&self, key: &str) -> Option<&ParamOptions> {
        self.param_options.get(key)
    }

    /// Declares what a vector parameter means on the canvas.
    pub fn with_param_role(mut self, key: impl Into<String>, role: ParamRole) -> Self {
        self.param_roles.insert(key.into(), role);
        self
    }

    pub fn param_role(&self, key: &str) -> Option<ParamRole> {
        self.param_roles.get(key).copied()
    }

    /// Declares a four-component parameter to be a colour.
    pub fn with_color_param(mut self, key: impl Into<String>) -> Self {
        self.color_params.insert(key.into(), ColorParam::Always);
        self
    }

    /// Declares a four-component parameter to be a colour only while the
    /// node's `on` parameter reads `equals` ([`ColorParam::When`]).
    pub fn with_color_param_when(
        mut self,
        key: impl Into<String>,
        on: impl Into<String>,
        equals: impl Into<String>,
    ) -> Self {
        self.color_params.insert(
            key.into(),
            ColorParam::When {
                key: on.into(),
                value: equals.into(),
            },
        );
        self
    }

    pub fn color_param(&self, key: &str) -> Option<&ColorParam> {
        self.color_params.get(key)
    }

    /// Declares one display group: `name` (the group's locale key is
    /// `node.<type_key>.group.<name>`) holding `keys`, appended after the
    /// groups declared before it.
    ///
    /// Nothing validates `keys` against [`Self::default_params`]: a key the
    /// type does not have is dropped where the sections are built, so a typo
    /// costs the parameter its group rather than the panel its rows.
    pub fn with_param_group<S: Into<String>>(
        mut self,
        name: impl Into<String>,
        keys: impl IntoIterator<Item = S>,
    ) -> Self {
        self.param_groups
            .push((name.into(), keys.into_iter().map(Into::into).collect()));
        self
    }

    /// The declared display groups, in section order.
    pub fn param_group_declarations(&self) -> &[(String, Vec<String>)] {
        &self.param_groups
    }

    /// Instantiate this template as a node with `id`.
    ///
    /// A `subnet` template instantiates its **inner graph** too
    /// ([`crate::network::seed_subnet_node`]) and takes its pins from that
    /// graph rather than from the template, which declares none: a subnet's
    /// interface is whatever its In / Out pair says it is (REQ-LAYER-003).
    /// That step mints two node ids, so it is the one part of node creation
    /// that is not a pure function of the template.
    pub fn create_node(&self, id: NodeId) -> Node {
        let mut node = Node::new(id, &self.type_key);
        node.inputs = self.inputs.clone();
        if let Some(base) = &self.variadic_input_group {
            let mut slot = base.clone();
            slot.is_param = false;
            slot.is_variadic = true;
            node.inputs.push(slot);
        }
        node.outputs = self.outputs.clone();
        node.parameters = self.default_params.clone();
        if let Some(label) = Some(&self.label) {
            node.metadata.label = Some(label.clone());
        }
        if self.type_key == crate::network::SUBNET_TYPE_KEY {
            crate::network::seed_subnet_node(&mut node);
        }
        node
    }
}

#[derive(Debug, Default)]
pub struct NodeRegistry {
    templates: HashMap<String, NodeTemplate>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, template: NodeTemplate) {
        self.templates.insert(template.type_key.clone(), template);
    }

    pub fn get(&self, type_key: &str) -> Option<&NodeTemplate> {
        self.templates.get(type_key)
    }

    pub fn create_node(&self, type_key: &str, id: NodeId) -> Option<Node> {
        self.templates.get(type_key).map(|t| t.create_node(id))
    }

    pub fn list_by_category(&self, category: NodeCategory) -> Vec<&NodeTemplate> {
        self.templates
            .values()
            .filter(|t| t.category == category)
            .collect()
    }

    pub fn all_templates(&self) -> impl Iterator<Item = &NodeTemplate> {
        self.templates.values()
    }

    /// Range metadata for `param_key` on `type_key`, if declared.
    pub fn param_range(&self, type_key: &str, param_key: &str) -> Option<&ParamRange> {
        self.templates.get(type_key)?.param_range(param_key)
    }

    /// Fixed closed option set for a string parameter, if declared as one.
    /// A contextual declaration answers `None` here — ask
    /// [`Self::param_option_source`] for it.
    pub fn param_options(&self, type_key: &str, param_key: &str) -> Option<&[String]> {
        self.templates.get(type_key)?.param_option_values(param_key)
    }

    /// Option-set declaration for a string parameter, fixed or contextual.
    pub fn param_option_source(&self, type_key: &str, param_key: &str) -> Option<&ParamOptions> {
        self.templates.get(type_key)?.param_option_source(param_key)
    }

    /// Geometric meaning of `param_key` on `type_key`, if declared.
    pub fn param_role(&self, type_key: &str, param_key: &str) -> Option<ParamRole> {
        self.templates.get(type_key)?.param_role(param_key)
    }

    pub fn categories(&self) -> Vec<NodeCategory> {
        let mut cats: Vec<_> = self
            .templates
            .values()
            .map(|t| t.category)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        cats.sort_by_key(|c| *c as u8);
        cats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::DataTypeId;

    // ----- contextual option sets (CPO-1) -----------------------------------

    /// A composition whose layer ids are deliberately **not** their row
    /// numbers, so a label built from the wrong one is visible.
    fn comp_with(names: &[&str]) -> Composition {
        let mut comp = Composition::new(
            crate::id::CompId::new(1),
            "Comp",
            (1920, 1080),
            crate::types::FrameRate::new(30, 1),
            300,
        );
        for (index, name) in names.iter().enumerate() {
            comp = comp.add_layer(
                Layer::new(
                    LayerId::new(index as u64 * 10 + 7),
                    *name,
                    crate::graph::Graph::new(),
                )
                .with_time(0, 0, 300),
            );
        }
        comp
    }

    /// The declaration a template written before `ParamOptions` existed makes
    /// is still a fixed set, and still reachable through the accessor every
    /// reader uses.
    #[test]
    fn with_param_options_declares_a_fixed_set() {
        let template = make_template().with_param_options("mode", ["a", "b"]);
        assert_eq!(
            template.param_option_source("mode"),
            Some(&ParamOptions::Fixed(vec!["a".into(), "b".into()]))
        );
        assert_eq!(template.param_option_values("mode").unwrap(), ["a", "b"]);
    }

    /// A contextual declaration has no values of its own, so the accessor the
    /// context-free readers use answers `None` rather than an empty list —
    /// "ask somewhere else", not "no candidates".
    #[test]
    fn a_contextual_declaration_has_no_fixed_values() {
        let template =
            make_template().with_contextual_param_options("layer", ContextualKind::SiblingLayer);
        assert_eq!(template.param_option_values("layer"), None);
        assert_eq!(
            template.param_option_source("layer"),
            Some(&ParamOptions::Contextual(ContextualKind::SiblingLayer))
        );

        let mut reg = NodeRegistry::new();
        reg.register(template);
        assert_eq!(reg.param_options("blur", "layer"), None);
        assert_eq!(
            reg.param_option_source("blur", "layer"),
            Some(&ParamOptions::Contextual(ContextualKind::SiblingLayer))
        );
    }

    /// The siblings in compositing order, the owner left out, and the label
    /// numbered by the layer's **Timeline row** — which counts from the top,
    /// the opposite end of `comp.layers` — and not by its place in the
    /// candidate list, which the excluded owner would shift.
    #[test]
    fn sibling_layer_options_exclude_the_owner_and_number_by_timeline_row() {
        let comp = comp_with(&["Background", "Middle", "Foreground"]);
        let options = contextual_options(
            ContextualKind::SiblingLayer,
            &probe_node(),
            &comp,
            Some(LayerId::new(17)),
        );
        assert_eq!(
            options,
            vec![
                ParamOption::new("7", "3. Background"),
                ParamOption::new("27", "1. Foreground"),
            ],
            "the bottom-most layer is the Timeline's last row, and Foreground \
             stays row 1 even though the candidate list now starts with it"
        );
    }

    /// A stale owner — a layer the composition no longer holds — is "no
    /// owning layer", not "every layer is a sibling".
    #[test]
    fn sibling_layer_options_are_empty_for_an_owner_the_composition_lost() {
        let comp = comp_with(&["Background", "Foreground"]);
        assert!(
            contextual_options(
                ContextualKind::SiblingLayer,
                &probe_node(),
                &comp,
                Some(LayerId::new(404))
            )
            .is_empty()
        );
    }

    /// A node whose parameters no contextual kind reads: the sibling
    /// candidates are decided by the composition and the owner alone, so the
    /// tests below feed the argument something rather than something
    /// particular.
    fn probe_node() -> Node {
        make_template().create_node(NodeId::new(1))
    }

    /// A node that belongs to no layer has no siblings: the list is empty and
    /// nothing panics looking for an owner that is not there.
    #[test]
    fn sibling_layer_options_are_empty_without_an_owning_layer() {
        let comp = comp_with(&["Background", "Foreground"]);
        assert!(
            contextual_options(ContextualKind::SiblingLayer, &probe_node(), &comp, None).is_empty()
        );
        assert!(
            contextual_options(
                ContextualKind::SiblingLayer,
                &probe_node(),
                &comp_with(&[]),
                Some(LayerId::new(7))
            )
            .is_empty(),
            "an empty composition offers nothing either"
        );
    }

    // ----- the referenced layer's output ports (CPO-3) ---------------------

    /// A composition of one layer whose network holds a `net.out` node with
    /// `ports` as its inputs — a layer's output ports are the Out node's
    /// *inputs* (REQ-LAYER-002/003).
    fn comp_with_out_ports(ports: &[(&str, DataTypeId)]) -> Composition {
        let mut out = Node::new(NodeId::new(2), crate::network::NET_OUT_TYPE_KEY);
        for (name, data_type) in ports {
            out = out.with_input(*name, &[*data_type]);
        }
        let network = crate::graph::Graph::new()
            .add_node(out)
            .expect("a fresh graph takes the Out node");
        Composition::new(
            crate::id::CompId::new(1),
            "Comp",
            (16, 16),
            crate::types::FrameRate::new(30, 1),
            300,
        )
        .add_layer(Layer::new(LayerId::new(7), "Target", network).with_time(0, 0, 300))
    }

    /// A `layer.ref` node pointing at `target`.
    fn layer_ref(target: &str) -> Node {
        let mut reg = NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let mut node = reg
            .create_node("layer.ref", NodeId::new(1))
            .expect("layer.ref is registered");
        for param in &mut node.parameters {
            if param.key == "layer" {
                param.value = crate::graph::ParameterValue::String(target.into());
            }
        }
        node
    }

    /// The candidates are the referenced layer's `net.out` inputs, in the
    /// order that node declares them, each reading as itself.
    #[test]
    fn layer_output_port_options_are_the_targets_out_node_inputs() {
        let comp = comp_with_out_ports(&[
            (crate::network::PORT_FRAME, DataTypeId::FRAME_BUFFER),
            ("geo", DataTypeId::GEOMETRY),
        ]);
        assert_eq!(
            contextual_options(
                ContextualKind::LayerOutputPort,
                &layer_ref("7"),
                &comp,
                Some(LayerId::new(9))
            ),
            vec![ParamOption::fixed("frame"), ParamOption::fixed("geo")],
            "the owner does not decide these — the node's own target does"
        );
    }

    /// Every way the reference fails to resolve offers nothing, which is what
    /// keeps the row from pointing somewhere the user did not choose.
    #[test]
    fn layer_output_port_options_are_empty_when_the_reference_resolves_to_nothing() {
        use crate::animation::step::StepCurve;

        let comp = comp_with_out_ports(&[(crate::network::PORT_FRAME, DataTypeId::FRAME_BUFFER)]);
        let empty = |node: &Node| {
            contextual_options(ContextualKind::LayerOutputPort, node, &comp, None).is_empty()
        };

        assert!(empty(&layer_ref("")), "no target picked yet");
        assert!(empty(&layer_ref("404")), "a layer this composition lost");
        assert!(empty(&layer_ref("not a number")), "a name, not an id");

        // A target that does not stand still names no layer at all.
        let mut moving = layer_ref("7");
        for param in &mut moving.parameters {
            if param.key == "layer" {
                let mut steps = StepCurve::keyed(0, "7".to_string());
                steps.insert(10, "9".to_string());
                param.value = crate::graph::ParameterValue::StringSteps(steps);
            }
        }
        assert!(empty(&moving), "an animated target names no layer");

        // A layer whose network has no Out node offers nothing either.
        let no_out = Composition::new(
            crate::id::CompId::new(1),
            "Comp",
            (16, 16),
            crate::types::FrameRate::new(30, 1),
            300,
        )
        .add_layer(Layer::new(
            LayerId::new(7),
            "Target",
            crate::graph::Graph::new(),
        ));
        assert!(
            contextual_options(
                ContextualKind::LayerOutputPort,
                &layer_ref("7"),
                &no_out,
                None
            )
            .is_empty()
        );
    }

    fn make_template() -> NodeTemplate {
        NodeTemplate::new("blur", "Gaussian Blur", NodeCategory::Image)
            .with_input(InputPort {
                name: "image".into(),
                accepted_types: vec![DataTypeId::FRAME_BUFFER],
                is_param: false,
                is_variadic: false,
            })
            .with_input(InputPort {
                name: "radius".into(),
                accepted_types: vec![DataTypeId::SCALAR],
                is_param: false,
                is_variadic: false,
            })
            .with_output(OutputPort {
                name: "output".into(),
                data_type: DataTypeId::FRAME_BUFFER,
            })
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = NodeRegistry::new();
        reg.register(make_template());
        assert!(reg.get("blur").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn create_node_from_template() {
        let mut reg = NodeRegistry::new();
        reg.register(make_template());
        let node = reg.create_node("blur", NodeId::new(1)).unwrap();
        assert_eq!(node.type_key, "blur");
        assert_eq!(node.inputs.len(), 2);
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.metadata.label.as_deref(), Some("Gaussian Blur"));
    }

    #[test]
    fn create_node_materializes_one_variadic_input_slot() {
        let template = NodeTemplate::new("merge", "Merge", NodeCategory::Geometry)
            .with_input(InputPort {
                name: "fixed".into(),
                accepted_types: vec![DataTypeId::GEOMETRY],
                is_param: false,
                is_variadic: false,
            })
            .with_variadic_input_group(InputPort {
                name: "source".into(),
                accepted_types: vec![DataTypeId::GEOMETRY],
                is_param: false,
                is_variadic: false,
            });

        let node = template.create_node(NodeId::new(9));

        assert_eq!(node.inputs.len(), 2);
        assert_eq!(node.inputs[0].name, "fixed");
        assert!(!node.inputs[0].is_variadic);
        assert_eq!(node.inputs[1].name, "source");
        assert!(node.inputs[1].is_variadic);
    }

    /// Adding a Subnet from the node palette must not produce a node the
    /// evaluator rejects: the template declares no ports, so `create_node`
    /// builds the inner In / Out pair the interface is derived from and takes
    /// its pins from there (REQ-LAYER-003).
    #[test]
    fn create_node_seeds_a_subnet_with_its_inner_network() {
        let mut reg = NodeRegistry::new();
        crate::registry::builtin::register_builtins(&mut reg);

        let node = reg
            .create_node(crate::network::SUBNET_TYPE_KEY, NodeId::new(1))
            .unwrap();

        let inner = node.subnet.as_deref().expect("a seeded inner graph");
        assert!(crate::network::find_in_node(inner).is_some());
        assert!(crate::network::find_out_node(inner).is_some());
        // `NodeId::new(1)` is where the counter starts, so this is the case
        // that would collide if seeding minted ids blindly. Ids are unique
        // across ownership levels — the evaluator's processor table is keyed
        // by `NodeId` with no path in it.
        assert!(
            !inner.node_ids().any(|id| id == node.id),
            "an inner node took the subnet node's own id"
        );
        assert!(node.inputs.is_empty());
        assert_eq!(
            node.outputs
                .iter()
                .map(|p| (p.name.as_str(), p.data_type))
                .collect::<Vec<_>>(),
            vec![(crate::network::PORT_FRAME, DataTypeId::FRAME_BUFFER)]
        );

        // Two subnets never share an inner node id.
        let other = reg
            .create_node(crate::network::SUBNET_TYPE_KEY, NodeId::new(2))
            .unwrap();
        let ids = |node: &Node| {
            let mut ids: Vec<_> = node.subnet.as_deref().unwrap().node_ids().collect();
            ids.sort();
            ids
        };
        assert_ne!(ids(&node), ids(&other));
    }

    #[test]
    fn list_by_category() {
        let mut reg = NodeRegistry::new();
        reg.register(make_template());
        reg.register(NodeTemplate::new(
            "constant",
            "Constant",
            NodeCategory::Utility,
        ));
        let images = reg.list_by_category(NodeCategory::Image);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].type_key, "blur");
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resolving an exposed parameter declaration's binding and applying a
//! caller's value to it (REQ-PROJ-006).
//!
//! [`super`] declares the contract; this module is the other half — turning
//! `name = value` into an edited [`Document`].
//!
//! # Once, before evaluation
//!
//! [`apply`] takes a document and returns a document. It is meant to run
//! **once**, before the evaluator is handed the result, and never during
//! evaluation. That is not a performance note: the evaluator caches by node
//! and graph identity, so a declaration resolved mid-evaluation would put a
//! value into a cached result that nothing in the cache key mentions. A
//! document that has been through `apply` is an ordinary document — every
//! consumer downstream sees only the parameters, not the declarations that
//! set them.
//!
//! # A declaration gives a default, it does not take over the parameter
//!
//! A parameter's value can come from any
//! [`ChannelSource`](crate::animation::channel::ChannelSource) — a constant,
//! keyframes, an expression, another node's output (REQ-CORE-007). A
//! declaration **stands where the constant is**: applying a value replaces the
//! constant a channel holds and leaves every other source alone. A keyframed
//! parameter keeps its keyframes, and the value the caller supplied is
//! reported back as unapplied ([`BindingIssueReason::AnimatedComponents`])
//! rather than silently dropped.
//!
//! The alternative — overwriting the channel with a constant — is the failure
//! this design exists to prevent: rendering a template with `--param
//! title=Hello` would delete the animation on the title. A caller that wants
//! to replace an animation is asking for something the external contract
//! deliberately cannot express (see the value-space discussion in [`super`]).
//!
//! Vectors are per component, so a `Vec2` whose `x` is keyframed and whose `y`
//! is constant takes the new `y` and keeps the animated `x`. **A partial write
//! is reported as well as performed**: the write lands, and the components
//! that kept their own source come back in [`Applied::issues`], because a
//! caller told only "applied" would read a
//! [`resolved`](crate::exposed::listing::ExposedListingEntry::resolved)
//! listing while half of its value never reached the render.
//!
//! # Only the names the caller supplied
//!
//! `apply` writes the declarations it was given values for. It does **not**
//! write every declaration's default: a default is what a caller may assume
//! when it supplies nothing, not a value the document has to be reset to.
//! Writing them all would mean that declaring a parameter freezes it at its
//! declaration-time value, so every later edit in the GUI would be undone by
//! the next render. [`crate::exposed::ExposedParameters`] is the listing a
//! caller reads defaults from ([`super`]).
//!
//! # A broken binding is reported, never fatal
//!
//! A binding names a node id and a parameter key, and the document is free to
//! move on: the node can be deleted, the parameter can be retyped, the key can
//! be renamed. A declaration whose binding no longer lands is **kept** — it is
//! part of the external contract, and dropping it would silently narrow that
//! contract — and reported as a [`BindingIssue`]. Applying such a value edits
//! nothing, so the resulting document evaluates exactly as it did before.
//! [`resolve`] answers the same question without applying anything, for a
//! caller that wants to check a contract before rendering with it.
//!
//! The one thing that *is* followed rather than reported is a parameter key
//! rename, because there the document knows exactly where the parameter went
//! ([`KeyRename`]).
//!
//! # Media is a reference, not a parameter value
//!
//! [`ExposedValue::Media`] has no [`ParameterValue`] counterpart: a media node
//! holds an `asset_id` into
//! [`Document::media_assets`](crate::composition::Document::media_assets), and
//! the file lives in that table. Applying one therefore does two things in the
//! same document: it registers an entry for the path the caller gave, named
//! `exposed:<declaration name>`, and it points the bound media node's
//! `asset_id` at that entry's [`AssetId`].
//!
//! Since `.ravprj` v9 the id is **minted**, so it cannot be derived from the
//! declaration. What makes applying the same value twice the same document is
//! the entry's own record of which declaration created it
//! ([`MediaAssetEntry::exposed_owner`]): a re-apply finds that entry and
//! reuses its id. The derived *name* ([`asset_name_for`]) is a label only —
//! the user may rename the asset in the MediaBin, and a re-apply keeps the
//! name it finds rather than resetting it. Ownership is never read out of the
//! name: names are editable and repeatable, so an asset renamed to
//! `exposed:foo` would otherwise be handed to the declaration `foo`.
//!
//! The entry the node used to reference is **left in the table**: another node,
//! or a layer's audio source, may still reference it, and apply is not a
//! garbage collector. By the same argument an entry the bound node does not
//! already reference is never reused
//! ([`ExposedApplyError::AssetIdTaken`]) — re-applying to one declaration is
//! a swap, but taking over an entry the node has moved off would swap media
//! nobody asked about.
//!
//! Three consequences worth stating exactly:
//!
//! * **an absent file is an error, not an empty frame.** The path is resolved
//!   through [`AssetPath::resolve`] — project-relative and `${VAR}` forms
//!   included (REQ-PROJ-005) — and it has to be a **file**, or the call is
//!   refused before anything is written. The alternative is a render that
//!   completes and is silently blank; a directory is refused here for the same
//!   reason, rather than surfacing as a decode failure later;
//! * **the extent of the new media is not compensated for.** Size and duration
//!   differences change nothing else: the composition keeps its resolution and
//!   duration, the layer keeps its in/out points and transform, and the network
//!   composites the new frames exactly as it composited the old ones. A
//!   template whose layout depended on which file the caller passed would not
//!   be a template. Frames past the new media's end behave like any other
//!   out-of-range read in that network;
//! * **no metadata is probed.** Width, height and duration need a decoder, and
//!   `ravel-core` has none, so the registered entry carries an empty
//!   [`AssetMetadata`](crate::composition::AssetMetadata) and a kind inferred
//!   from the file extension. A caller that needs the real extents probes and
//!   re-registers. A numbered image **sequence** cannot be declared for the
//!   same reason: its `AssetKind` carries fields only the import probe knows,
//!   so a declared path is a container or a still.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::animation::channel::{AnimationChannel, ChannelSource};
use crate::composition::{
    AssetKind, AssetPath, Composition, Document, MEDIA_ASSET_PARAM_KEY, MEDIA_TYPE_KEYS,
    MediaAssetEntry, graph_walk,
};
use crate::eval::EvalContext;
use crate::exposed::{ExposedBinding, ExposedParameter, ExposedType, ExposedValue, KeyRename};
use crate::graph::{Graph, Node, Parameter, ParameterValue};
use crate::id::{AssetId, NodeId};
use crate::types::{Color, FrameRate, Vec2, Vec3, Vec4};

// ===========================================================================
// Reporting
// ===========================================================================

/// Why a declaration's binding does not (fully) drive its parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingIssueReason {
    /// No node in the document carries the bound id — it was deleted, or the
    /// layer that owned its network was.
    NodeMissing,
    /// The node is there but declares no parameter under the bound key.
    ParameterMissing,
    /// The parameter is there but holds a kind the declared type cannot
    /// drive — the parameter was retyped, or the declaration was written
    /// against a different node.
    KindMismatch {
        declared: ExposedType,
        /// The parameter's current kind, as
        /// [`ParameterValue`]'s variant name.
        parameter_kind: &'static str,
    },
    /// The parameter is driven by something other than a constant on these
    /// component indices, so the declaration does not set them (see the module
    /// documentation). A scalar parameter reports `[0]`.
    AnimatedComponents { components: Vec<usize> },
    /// A media declaration binds to a media node's asset reference, and the
    /// node it names is not a media node. Reported rather than written,
    /// because writing an asset id into some other node's string parameter
    /// would corrupt that parameter instead of swapping any media.
    NotAMediaNode { type_key: String },
    /// The node is a media node, but the bound key is not the parameter it
    /// reads its asset from. Every other string parameter on a media node is
    /// something else, and writing an asset id into one of those would report
    /// a swap that the processor never sees — the picture would not change
    /// and the parameter that did change would be corrupt.
    NotAnAssetReference {
        /// The key a media declaration has to bind to.
        expected: &'static str,
    },
}

/// One declaration's binding, and what is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingIssue {
    /// The declaration's contract name.
    pub name: String,
    /// The node the binding names.
    pub node: NodeId,
    /// The parameter key the binding names.
    pub key: String,
    pub reason: BindingIssueReason,
}

impl std::fmt::Display for BindingIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            name,
            node,
            key,
            reason,
        } = self;
        match reason {
            BindingIssueReason::NodeMissing => {
                write!(
                    f,
                    "exposed parameter {name:?} is bound to {node:?}, which the document no longer has"
                )
            }
            BindingIssueReason::ParameterMissing => {
                write!(
                    f,
                    "exposed parameter {name:?} is bound to {key:?} on {node:?}, which has no such parameter"
                )
            }
            BindingIssueReason::KindMismatch {
                declared,
                parameter_kind,
            } => {
                write!(
                    f,
                    "exposed parameter {name:?} declares type {declared} but {key:?} on {node:?} is a {parameter_kind} parameter"
                )
            }
            BindingIssueReason::AnimatedComponents { components } => {
                write!(
                    f,
                    "exposed parameter {name:?} does not set {components:?} of {key:?} on {node:?}: they are animated, not constant"
                )
            }
            BindingIssueReason::NotAMediaNode { type_key } => {
                write!(
                    f,
                    "exposed parameter {name:?} declares a media reference but {node:?} is a {type_key:?} node, not a media node"
                )
            }
            BindingIssueReason::NotAnAssetReference { expected } => {
                write!(
                    f,
                    "exposed parameter {name:?} declares a media reference but is bound to {key:?} on {node:?}, which is not that node's asset reference ({expected:?})"
                )
            }
        }
    }
}

/// Why a caller's values were refused, before anything was written.
///
/// Every variant is a mistake in the *call*, not in the document — a document
/// whose bindings no longer land yields a [`BindingIssue`] instead, and still
/// applies. Validation runs over the whole set of values before the first
/// write, so a refused call leaves the document exactly as it was.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ExposedApplyError {
    #[error("no exposed parameter named {0:?} is declared")]
    Undeclared(String),

    #[error("exposed parameter {name:?} takes {declared} but the value given is {found}")]
    TypeMismatch {
        name: String,
        declared: ExposedType,
        found: ExposedType,
    },

    #[error("exposed parameter {0:?} was given a non-finite value")]
    NonFiniteValue(String),

    /// The reference has no location: an unsaved project cannot anchor a
    /// relative path, and an unknown `${VAR}` expands to nothing.
    #[error(
        "exposed parameter {name:?} was given the media reference {path}, which does not resolve to a location"
    )]
    MediaUnresolved { name: String, path: AssetPath },

    #[error(
        "exposed parameter {name:?} already owns the media asset {asset:?} ({existing}), which the node it is bound to does not reference"
    )]
    AssetIdTaken {
        name: String,
        /// The display name of the entry in the way, not its id: the id is
        /// minted here and would tell the caller nothing about which asset it
        /// has to sort out.
        asset: String,
        existing: AssetPath,
    },

    /// Two entries claim the same declaration
    /// ([`MediaAssetEntry::exposed_owner`]) — a state only a hand-edited or
    /// hand-merged document can reach. Picking one silently would overwrite
    /// media the caller never named.
    #[error(
        "exposed parameter {name:?} owns {count} media assets, so which one it would replace is undecidable"
    )]
    AssetOwnerAmbiguous { name: String, count: usize },

    #[error(
        "exposed parameter {name:?} was given the media reference {path}, and there is no file at {resolved}"
    )]
    MediaNotFound {
        name: String,
        path: AssetPath,
        resolved: PathBuf,
    },
}

/// Where a media reference resolves against (REQ-PROJ-005).
///
/// The same pair
/// [`Document::with_resolved_assets`](crate::composition::Document::with_resolved_assets)
/// takes: the project root a `./relative` path is anchored to, and the
/// substitutions a `${VAR}` path expands with (`PROJECT_ROOT` is supplied from
/// the root). The default — no root, no variables — is what a caller that
/// declares no media needs, and what an unsaved project has.
#[derive(Clone, Copy, Debug, Default)]
pub struct AssetContext<'a> {
    pub project_root: Option<&'a Path>,
    pub vars: Option<&'a HashMap<String, String>>,
}

impl<'a> AssetContext<'a> {
    pub fn new(project_root: Option<&'a Path>, vars: &'a HashMap<String, String>) -> Self {
        Self {
            project_root,
            vars: Some(vars),
        }
    }

    /// Anchor relative paths at `project_root`, with no extra variables.
    pub fn rooted(project_root: &'a Path) -> Self {
        Self {
            project_root: Some(project_root),
            vars: None,
        }
    }

    fn resolve(&self, path: &AssetPath) -> Option<PathBuf> {
        let empty = HashMap::new();
        path.resolve(self.project_root, self.vars.unwrap_or(&empty))
    }
}

/// The document a set of values produced, and everything that did not land.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The edited document. Identical to the input when nothing applied.
    pub document: Document,
    /// One entry per supplied declaration whose binding did not (fully) take
    /// the value, in declaration order.
    pub issues: Vec<BindingIssue>,
}

// ===========================================================================
// Resolution
// ===========================================================================

/// Report every declaration in `document` whose binding would not (fully)
/// take its own default value, in declaration order.
///
/// This is the contract check a caller can run before committing to a render:
/// an empty result means every declared name reaches a parameter it can drive.
/// It answers for the declared **type**, so it is independent of any
/// particular value.
pub fn resolve(document: &Document) -> Vec<BindingIssue> {
    document
        .exposed_parameters
        .iter()
        .filter_map(|declaration| {
            match inspect(document, declaration, declaration.default_value(), None) {
                Err(issue) => Some(issue),
                // A binding that only half lands is reported too: a caller
                // reading a listing has to be able to tell a name whose value
                // takes effect from one whose value takes effect on two of
                // four components.
                Ok(inspected) => inspected.unapplied,
            }
        })
        .collect()
}

/// The value a declaration binding to `binding` should default to: the value
/// that parameter has in `document` at `frame`.
///
/// This is the other half of "expose this parameter": an editor knows the node
/// and the key the user clicked, and needs the *declaration* that describes it
/// — which type it belongs to, and what the caller gets when they supply
/// nothing. Deriving both here rather than in the editor keeps one mapping
/// between a [`ParameterValue`] and an [`ExposedValue`] in the codebase; a
/// second one written in a panel would decide the type differently and mint
/// declarations [`apply`] refuses.
///
/// The type it picks is the one [`assign`] writes back through:
///
/// | parameter | seeded as |
/// |---|---|
/// | `Float` / `Int` / `Bool` / `String` | the same constant |
/// | `Channel` | [`ExposedType::Float`] |
/// | `Channel2` / `Channel3` | [`ExposedType::Vec2`] / [`ExposedType::Vec3`] |
/// | `Channel4` | [`ExposedType::Color`] — what a four-channel parameter is presented as |
/// | a media node's `asset_id` | [`ExposedType::Media`], carrying the asset's current path |
///
/// `None` means the parameter has no place in an external contract at all:
/// the node or the key is gone, the value is a `PathPoints` or a `Curve` (the
/// exclusions the module documentation states), or it is a media node's
/// `asset_id` naming an asset the document does not hold — there is no path to
/// default to, and inventing an empty one would declare a contract that
/// resolves to nothing.
///
/// # This is not the inverse of [`assign`]
///
/// The type it seeds is the one `assign` writes back through, but a value it
/// seeds is not one `assign` will necessarily write: a component driven by
/// keyframes or an expression can be *read* and cannot be *written*, so a
/// declaration seeded over one lands only partially (or not at all) and
/// [`resolve`] reports it as
/// [`BindingIssueReason::AnimatedComponents`]. Declaring such a parameter is
/// still worth allowing — the contract lists the input and says why it does
/// not take — so the refusal belongs in the report, not here.
///
/// **`frame` is what makes that default a number worth having.** An animated
/// component is sampled there, so the declaration's default is the value the
/// render would produce at that frame rather than an unconditional `0.0` that
/// a caller omitting `--param` would silently get. Callers pass the frame the
/// user is looking at (in the editor, the playhead's layer-local frame). A
/// source that has no value yet — `NodeOutput`, `AudioReactive` — samples as
/// [`ChannelSource::DEFAULT_VALUE`], the same value a render reads from it.
pub fn seed_value(
    document: &Document,
    binding: &ExposedBinding,
    frame: u64,
) -> Option<ExposedValue> {
    let (node, comp) = find_node_with_basis(document, binding.node)?;
    let current = node
        .parameters
        .iter()
        .find(|parameter| parameter.key == binding.key)?;

    if MEDIA_TYPE_KEYS.contains(&node.type_key.as_str()) && binding.key == MEDIA_ASSET_PARAM_KEY {
        let ParameterValue::String(asset) = &current.value else {
            return None;
        };
        return AssetId::from_param_value(asset)
            .and_then(|id| document.get_media_asset(id))
            .map(|entry| ExposedValue::Media(entry.path.clone()));
    }

    let ctx = seed_context(document, comp, frame);
    let at = |channel: &AnimationChannel| sample_for_seed(channel, &ctx);

    Some(match &current.value {
        ParameterValue::Float(v) => ExposedValue::Float(*v),
        ParameterValue::Int(v) => ExposedValue::Int(*v),
        ParameterValue::Bool(v) => ExposedValue::Bool(*v),
        ParameterValue::String(v) => ExposedValue::String(v.clone()),
        ParameterValue::Channel(c) => ExposedValue::Float(at(c)),
        // An animatable int seeds the int it reads as at this frame, the same
        // rounding the evaluator applies.
        ParameterValue::IntChannel(c) => ExposedValue::Int(at(c).round() as i32),
        // Likewise for an animatable string: the seed is the string this frame
        // reads, which is the one the render would use.
        ParameterValue::StringSteps(steps) => {
            ExposedValue::String(steps.sample(frame as f64).clone())
        }
        ParameterValue::Channel2(c) => ExposedValue::Vec2(Vec2(at(&c[0]), at(&c[1]))),
        ParameterValue::Channel3(c) => ExposedValue::Vec3(Vec3(at(&c[0]), at(&c[1]), at(&c[2]))),
        ParameterValue::Channel4(c) => ExposedValue::Color(Color {
            r: at(&c[0]),
            g: at(&c[1]),
            b: at(&c[2]),
            a: at(&c[3]),
        }),
        ParameterValue::PathPoints(_) | ParameterValue::Curve(_) | ParameterValue::Ramp(_) => {
            return None;
        }
    })
}

/// The value `channel` has at `ctx`'s frame, whatever drives it.
///
/// A non-finite sample falls back to `0.0`: [`ExposedParameter::new`] refuses a
/// non-finite default, so passing one through would turn "expose this
/// parameter" into an error the user cannot act on.
fn sample_for_seed(channel: &AnimationChannel, ctx: &EvalContext) -> f32 {
    let value = channel.evaluate(ctx.sample_frame(), ctx);
    if value.is_finite() { value } else { 0.0 }
}

/// The basis an animated component is sampled against: `comp`'s frame rate and
/// resolution at `frame`.
///
/// A node in the document's flat graph belongs to no composition, so the root
/// composition's basis stands in — it is the only timeline that graph could be
/// rendered under. With no composition at all there is nothing to stand in for;
/// keyframes still sample correctly (they are indexed in frames) and only an
/// expression reading the canvas sees the placeholder.
fn seed_context(document: &Document, comp: Option<&Composition>, frame: u64) -> EvalContext {
    let comp = comp.or_else(|| {
        document
            .root_comp
            .and_then(|id| document.get_composition(id))
            .map(|comp| &**comp)
    });
    match comp {
        Some(comp) => EvalContext::new(frame, comp.frame_rate, comp.resolution),
        None => EvalContext::new(frame, FrameRate::new(30, 1), (1, 1)),
    }
}

/// What inspecting one declaration found: the write it produces, and the part
/// of the value that did not land.
struct Inspected {
    write: Write,
    /// Set when the binding lands but the value does not, wholly — today only
    /// [`BindingIssueReason::AnimatedComponents`] on a partial write. `None`
    /// when the whole value took effect.
    unapplied: Option<BindingIssue>,
}

/// What applying one declaration amounts to.
enum Write {
    /// Store this parameter on this node.
    Parameter(NodeId, Parameter),
    /// Register `entry` and point the media node's parameter at it — the two
    /// halves of a media swap, which land together or not at all.
    ///
    /// The id is not decided here: it is either the one the declaration
    /// already owns or a freshly minted one, and telling those apart needs the
    /// whole document, which [`apply`] has and [`inspect`] deliberately only
    /// reads.
    Media {
        node: NodeId,
        key: String,
        /// The asset the node references today, if it references one at all.
        /// An entry the declaration does not already own is not overwritten:
        /// doing so would take an unrelated asset with it.
        current: Option<AssetId>,
        entry: Box<MediaAssetEntry>,
    },
    /// The binding is sound and there is nothing to store: a media
    /// declaration inspected without a resolved location, which is how
    /// [`resolve`] asks whether a binding lands without owning a file.
    Nothing,
}

/// Look the declaration's binding up in `document` and work out what writing
/// `value` to it would do.
///
/// `resolved_media` is the absolute location a media value was resolved to
/// (see [`AssetContext`]); `None` means the caller is only asking whether the
/// binding lands. `Err` carries the reason nothing lands.
fn inspect(
    document: &Document,
    declaration: &ExposedParameter,
    value: &ExposedValue,
    resolved_media: Option<&Path>,
) -> Result<Inspected, BindingIssue> {
    let binding = declaration.binding();
    let issue = |reason| BindingIssue {
        name: declaration.name().to_string(),
        node: binding.node,
        key: binding.key.clone(),
        reason,
    };

    let node =
        find_node(document, binding.node).ok_or_else(|| issue(BindingIssueReason::NodeMissing))?;
    let current = node
        .parameters
        .iter()
        .find(|parameter| parameter.key == binding.key)
        .ok_or_else(|| issue(BindingIssueReason::ParameterMissing))?;

    if let ExposedValue::Media(path) = value {
        if !MEDIA_TYPE_KEYS.contains(&node.type_key.as_str()) {
            return Err(issue(BindingIssueReason::NotAMediaNode {
                type_key: node.type_key.clone(),
            }));
        }
        // Being a string is not the same as being the asset reference: a
        // media node can carry other string parameters, and writing an asset
        // id into one of those changes nothing the processor reads while
        // corrupting the parameter it does hit. The key has to be the one the
        // processor looks the asset up by.
        if binding.key != MEDIA_ASSET_PARAM_KEY {
            return Err(issue(BindingIssueReason::NotAnAssetReference {
                expected: MEDIA_ASSET_PARAM_KEY,
            }));
        }
        // The asset reference itself holding something other than a string is
        // a media node whose reference has been replaced by something that is
        // not one.
        let ParameterValue::String(names_today) = &current.value else {
            return Err(issue(BindingIssueReason::KindMismatch {
                declared: declaration.value_type(),
                parameter_kind: parameter_kind(&current.value),
            }));
        };
        // A reference that is not an id at all — the template default `""`,
        // or a pre-v9 name in a document assembled by hand — names no asset,
        // which is the same starting point as a node nobody has pointed
        // anywhere yet.
        let references_today = AssetId::from_param_value(names_today);
        let Some(resolved) = resolved_media else {
            return Ok(Inspected {
                write: Write::Nothing,
                unapplied: None,
            });
        };
        return Ok(Inspected {
            write: Write::Media {
                node: binding.node,
                key: binding.key.clone(),
                current: references_today,
                entry: Box::new(MediaAssetEntry {
                    name: asset_name_for(declaration.name()),
                    path: path.clone(),
                    kind: AssetKind::infer_from_path(resolved),
                    metadata: Default::default(),
                    color_space: None,
                    // What makes the entry this declaration's, and the only
                    // thing `apply` reads to find it again.
                    exposed_owner: Some(declaration.name().to_string()),
                    resolved: Some(resolved.to_path_buf()),
                }),
            },
            unapplied: None,
        });
    }

    let assignment = assign(value, &current.value).ok_or_else(|| {
        issue(BindingIssueReason::KindMismatch {
            declared: declaration.value_type(),
            parameter_kind: parameter_kind(&current.value),
        })
    })?;

    match assignment {
        Assignment::Written(written, blocked) => Ok(Inspected {
            write: Write::Parameter(
                binding.node,
                Parameter {
                    key: binding.key.clone(),
                    value: written,
                },
            ),
            unapplied: (!blocked.is_empty()).then(|| {
                issue(BindingIssueReason::AnimatedComponents {
                    components: blocked,
                })
            }),
        }),
        Assignment::Blocked(components) => {
            Err(issue(BindingIssueReason::AnimatedComponents { components }))
        }
    }
}

/// The display name a media declaration gives an asset it creates.
///
/// A label, and nothing else: which entry a declaration owns is recorded on
/// the entry itself ([`MediaAssetEntry::exposed_owner`]), because a name can
/// be edited in the MediaBin and two assets can carry the same one. Nothing
/// here is looked up, and a re-apply leaves whatever name the entry now has
/// alone — only a freshly minted entry is named from this.
///
/// The prefix keeps it clear of the file-stem names the import path mints, and
/// says which declaration put the entry in a saved project.
fn asset_name_for(name: &str) -> String {
    format!("{EXPOSED_ASSET_NAME_PREFIX}{name}")
}

/// The prefix [`asset_name_for`] puts in front of the declaration name.
///
/// Named because the v8 → v9 upgrade has to read it: before
/// [`MediaAssetEntry::exposed_owner`] existed, that name was the *only* record
/// of which declaration had created an entry
/// (`super::super::composition::asset_upgrade`).
pub(crate) const EXPOSED_ASSET_NAME_PREFIX: &str = "exposed:";

// ===========================================================================
// Application
// ===========================================================================

/// Apply `values` — a caller's `name = value` pairs — to `document`, resolving
/// media references against `assets`.
///
/// Runs **once**, before evaluation (see the module documentation). Every
/// value is validated against its declaration first — including that a media
/// reference resolves to a file that exists — so a call that names an
/// undeclared parameter, hands it the wrong type, or points at media that is
/// not there writes nothing at all. Bindings that no longer land are reported
/// in [`Applied::issues`] and cost the document nothing.
///
/// Pass `AssetContext::default()` when no declaration is a media reference.
pub fn apply(
    document: Document,
    values: &HashMap<String, ExposedValue>,
    assets: AssetContext<'_>,
) -> Result<Applied, ExposedApplyError> {
    let declarations = document.exposed_parameters.clone();

    // Reject the call before touching anything. Unknown names first — a
    // caller that misspelled a name has not made a type mistake — then the
    // rest in declaration order, so the error a caller sees does not depend on
    // a hash map's iteration order.
    let mut undeclared: Vec<&String> = values
        .keys()
        .filter(|name| !declarations.contains(name))
        .collect();
    undeclared.sort();
    if let Some(name) = undeclared.first() {
        return Err(ExposedApplyError::Undeclared((*name).clone()));
    }
    for declaration in declarations.iter() {
        let Some(value) = values.get(declaration.name()) else {
            continue;
        };
        let found = value.exposed_type();
        if found != declaration.value_type() {
            return Err(ExposedApplyError::TypeMismatch {
                name: declaration.name().to_string(),
                declared: declaration.value_type(),
                found,
            });
        }
        if !value.is_finite() {
            return Err(ExposedApplyError::NonFiniteValue(
                declaration.name().to_string(),
            ));
        }
    }

    // Media is resolved in the same pass, for the same reason: a render that
    // starts and produces blank frames is worse than one that refuses to
    // start. The locations are kept because resolving twice could disagree.
    let mut located: HashMap<&str, PathBuf> = HashMap::new();
    for declaration in declarations.iter() {
        let Some(ExposedValue::Media(path)) = values.get(declaration.name()) else {
            continue;
        };
        let resolved = assets
            .resolve(path)
            .ok_or_else(|| ExposedApplyError::MediaUnresolved {
                name: declaration.name().to_string(),
                path: path.clone(),
            })?;
        // `is_file`, not `exists`: a directory exists and is not media, and
        // handing one to the decoder turns a wrong argument into a failure
        // much further from the caller.
        if !resolved.is_file() {
            return Err(ExposedApplyError::MediaNotFound {
                name: declaration.name().to_string(),
                path: path.clone(),
                resolved,
            });
        }
        located.insert(declaration.name(), resolved);
    }

    let mut writes: HashMap<NodeId, Vec<Parameter>> = HashMap::new();
    // Collected rather than installed as they are found: a refusal below has
    // to leave the document exactly as it arrived.
    let mut assets: Vec<(AssetId, MediaAssetEntry)> = Vec::new();
    let mut issues = Vec::new();
    for declaration in declarations.iter() {
        let Some(value) = values.get(declaration.name()) else {
            continue;
        };
        let resolved = located.get(declaration.name()).map(PathBuf::as_path);
        match inspect(&document, declaration, value, resolved) {
            Ok(inspected) => {
                issues.extend(inspected.unapplied);
                match inspected.write {
                    Write::Parameter(node, parameter) => {
                        writes.entry(node).or_default().push(parameter)
                    }
                    Write::Media {
                        node,
                        key,
                        current,
                        entry,
                    } => {
                        // The entry this declaration left on an earlier apply,
                        // found by the declaration it records
                        // (`MediaAssetEntry::exposed_owner`) and never by its
                        // name: the name is the user's to edit, so a document
                        // can hold two assets called the same thing and any
                        // asset can be renamed to look like a declaration's.
                        let owned: Vec<(AssetId, &MediaAssetEntry)> = document
                            .media_assets
                            .iter()
                            .filter(|(_, entry)| {
                                entry.exposed_owner.as_deref() == Some(declaration.name())
                            })
                            .map(|(id, entry)| (*id, entry))
                            .collect();
                        if owned.len() > 1 {
                            // Only a hand-edited document reaches this, and
                            // choosing between the claimants would overwrite
                            // an asset on a coin toss.
                            return Err(ExposedApplyError::AssetOwnerAmbiguous {
                                name: declaration.name().to_string(),
                                count: owned.len(),
                            });
                        }
                        let mut entry = *entry;
                        // Reusing an entry the bound node does not already
                        // reference would replace media that node has nothing
                        // to do with — and take every other node and audio
                        // source reading it along. Re-applying to the same
                        // declaration is the one case where the entry is
                        // legitimately in use: the node references it because
                        // a previous apply put it there.
                        let asset = match owned.first() {
                            Some((id, existing)) if current == Some(*id) => {
                                // The path is the declaration's to write; the
                                // display name is the user's. A rename made in
                                // the MediaBin survives every later apply.
                                entry.name = existing.name.clone();
                                *id
                            }
                            Some((_, existing)) => {
                                return Err(ExposedApplyError::AssetIdTaken {
                                    name: declaration.name().to_string(),
                                    asset: existing.name.clone(),
                                    existing: existing.path.clone(),
                                });
                            }
                            // Nothing claims this declaration yet, so it has
                            // never been applied to this document: mint.
                            None => AssetId::next(),
                        };
                        writes.entry(node).or_default().push(Parameter {
                            key,
                            value: ParameterValue::String(asset.to_param_value()),
                        });
                        assets.push((asset, entry));
                    }
                    Write::Nothing => {}
                }
            }
            Err(issue) => issues.push(issue),
        }
    }

    let mut document = document;
    for (asset, entry) in assets {
        document = document.with_media_asset_entry(asset, entry);
    }
    let document = if writes.is_empty() {
        document
    } else {
        write_parameters(document, &writes)
    };
    Ok(Applied { document, issues })
}

/// Carry `rename` into `document`'s declarations: every binding that named the
/// key it moved names the new key afterwards.
///
/// The counterpart of [`crate::network::rename_custom_port`], which produces
/// the [`KeyRename`]. It belongs in the **same document commit** as the graph
/// the rename edited: a commit that carries one without the other leaves a
/// declaration bound to a key nothing has, which is precisely the fragility
/// binding by node id was chosen to avoid.
pub fn follow_key_rename(document: Document, rename: &KeyRename) -> Document {
    let mut declarations = document.exposed_parameters.clone();
    if declarations.follow_key_rename(rename) == 0 {
        return document;
    }
    document.with_exposed_parameters(declarations)
}

// ===========================================================================
// Value assignment
// ===========================================================================

/// What writing an [`ExposedValue`] over a [`ParameterValue`] amounts to.
enum Assignment {
    /// The parameter to store, and the component indices that kept what they
    /// had because they are not constant. A partial write carries a non-empty
    /// list: the value landed, but not all of it, and saying so is the
    /// caller's only way to tell a whole application from half of one.
    Written(ParameterValue, Vec<usize>),
    /// Nothing to store: every component named here is animated.
    Blocked(Vec<usize>),
}

/// Merge `value` into `current`, or `None` when the declared value simply is
/// not a value of that parameter's kind.
///
/// The pairing is deliberately narrow. `Float` reaches both a plain constant
/// `Float` parameter and a one-channel one, because those are the same
/// quantity stored two ways, but nothing else widens: an `Int` does not fill a
/// `Float`, a `Vec3` does not fill a `Channel4`. A contract that quietly
/// converts is a contract whose meaning depends on the internals it was
/// designed not to expose.
///
/// [`ExposedType::Media`] has no pairing here: a media reference is not a
/// value a parameter holds but an entry in the document's asset table, which
/// is EXPO-4's job (`docs/implementation/done/exposed-parameters-plan.md`).
fn assign(value: &ExposedValue, current: &ParameterValue) -> Option<Assignment> {
    match (value, current) {
        (ExposedValue::Float(v), ParameterValue::Float(_)) => {
            Some(Assignment::Written(ParameterValue::Float(*v), Vec::new()))
        }
        (ExposedValue::Int(v), ParameterValue::Int(_)) => {
            Some(Assignment::Written(ParameterValue::Int(*v), Vec::new()))
        }
        (ExposedValue::Bool(v), ParameterValue::Bool(_)) => {
            Some(Assignment::Written(ParameterValue::Bool(*v), Vec::new()))
        }
        (ExposedValue::String(v), ParameterValue::String(_)) => Some(Assignment::Written(
            ParameterValue::String(v.clone()),
            Vec::new(),
        )),
        (ExposedValue::Float(v), ParameterValue::Channel(channel)) => {
            Some(channels(&[*v], std::slice::from_ref(channel), |written| {
                ParameterValue::Channel(written[0].clone())
            }))
        }
        // Same widening as `Float` → `Channel`, for the same reason: a
        // constant `Int` and an `IntChannel` are one quantity stored two ways,
        // so a declaration that reaches the constant must reach the animated
        // spelling too (a keyframe on the target would otherwise make the
        // declaration silently stop resolving).
        (ExposedValue::Int(v), ParameterValue::IntChannel(channel)) => Some(channels(
            &[*v as f32],
            std::slice::from_ref(channel),
            |written| ParameterValue::IntChannel(written[0].clone()),
        )),
        // An animatable string is reachable by a declaration bound to it — the
        // keyframe toggle re-types the parameter under a declaration that
        // already exists, and the binding must not stop resolving — but the
        // write is refused rather than applied. A step curve has no constant
        // half to write into: every frame's value is a key the user placed,
        // and overwriting them all is not what "set this parameter" means.
        // Reporting it blocked is the same answer a fully keyframed channel
        // gets from `channels` below.
        (ExposedValue::String(_), ParameterValue::StringSteps(_)) => {
            Some(Assignment::Blocked(vec![0]))
        }
        (ExposedValue::Vec2(Vec2(x, y)), ParameterValue::Channel2(channels_now)) => {
            Some(channels(&[*x, *y], channels_now, |written| {
                ParameterValue::Channel2([written[0].clone(), written[1].clone()])
            }))
        }
        (ExposedValue::Vec3(Vec3(x, y, z)), ParameterValue::Channel3(channels_now)) => {
            Some(channels(&[*x, *y, *z], channels_now, |written| {
                ParameterValue::Channel3([
                    written[0].clone(),
                    written[1].clone(),
                    written[2].clone(),
                ])
            }))
        }
        (ExposedValue::Vec4(Vec4(x, y, z, w)), ParameterValue::Channel4(channels_now)) => {
            Some(channels(&[*x, *y, *z, *w], channels_now, |written| {
                ParameterValue::Channel4([
                    written[0].clone(),
                    written[1].clone(),
                    written[2].clone(),
                    written[3].clone(),
                ])
            }))
        }
        (ExposedValue::Color(Color { r, g, b, a }), ParameterValue::Channel4(channels_now)) => {
            Some(channels(&[*r, *g, *b, *a], channels_now, |written| {
                ParameterValue::Channel4([
                    written[0].clone(),
                    written[1].clone(),
                    written[2].clone(),
                    written[3].clone(),
                ])
            }))
        }
        _ => None,
    }
}

/// Write `values` over the constant components of `current`, leaving every
/// other source untouched (see the module documentation).
fn channels(
    values: &[f32],
    current: &[AnimationChannel],
    rebuild: impl Fn(&[AnimationChannel]) -> ParameterValue,
) -> Assignment {
    let mut written = Vec::with_capacity(current.len());
    let mut blocked = Vec::new();
    for (index, (channel, value)) in current.iter().zip(values).enumerate() {
        if matches!(channel.source, ChannelSource::Constant(_)) {
            written.push(AnimationChannel::constant(*value));
        } else {
            written.push(channel.clone());
            blocked.push(index);
        }
    }
    if blocked.len() == current.len() {
        return Assignment::Blocked(blocked);
    }
    // A partial write is still a write — the constant half took the value —
    // but the animated half did not, and that travels with it. A caller told
    // only "applied" would read a listing that says `resolved` and a render
    // that used its own value for half the components.
    Assignment::Written(rebuild(&written), blocked)
}

/// A parameter's kind, for a report a human reads.
fn parameter_kind(value: &ParameterValue) -> &'static str {
    match value {
        ParameterValue::Float(_) => "float",
        ParameterValue::Int(_) => "int",
        ParameterValue::Bool(_) => "bool",
        ParameterValue::String(_) => "string",
        ParameterValue::Channel(_) => "channel",
        ParameterValue::Channel2(_) => "channel2",
        ParameterValue::Channel3(_) => "channel3",
        ParameterValue::Channel4(_) => "channel4",
        ParameterValue::PathPoints(_) => "path points",
        ParameterValue::Curve(_) => "curve",
        ParameterValue::Ramp(_) => "ramp",
        ParameterValue::IntChannel(_) => "int channel",
        ParameterValue::StringSteps(_) => "string steps",
    }
}

// ===========================================================================
// Document traversal
// ===========================================================================

/// The node `id` names, wherever in the document it lives: the flat graph,
/// a layer's network, or any subnet at any depth.
///
/// Node ids are document-globally unique (REQ-LAYER-009), which is what makes
/// a binding a stable reference in the first place — and what lets this search
/// stop at the first hit.
fn find_node(document: &Document, id: NodeId) -> Option<&Arc<Node>> {
    find_node_with_basis(document, id).map(|(node, _)| node)
}

/// The same search, also reporting the composition the node was found in —
/// the frame rate and resolution [`seed_value`] samples an animated component
/// against. `None` for the composition means the document's flat graph, which
/// belongs to no composition.
fn find_node_with_basis(
    document: &Document,
    id: NodeId,
) -> Option<(&Arc<Node>, Option<&Composition>)> {
    if let Some(node) = node_in(&document.graph, id) {
        return Some((node, None));
    }
    document.compositions.values().find_map(|comp| {
        comp.layers
            .iter()
            .find_map(|layer| node_in(&layer.network, id))
            .map(|node| (node, Some(&**comp)))
    })
}

fn node_in(graph: &Graph, id: NodeId) -> Option<&Arc<Node>> {
    if let Some(node) = graph.node(id) {
        return Some(node);
    }
    graph
        .nodes()
        .filter_map(|node| node.subnet.as_deref())
        .find_map(|inner| node_in(inner, id))
}

/// Store `writes` — parameters by node id — in one pass over every graph the
/// document owns.
///
/// One pass rather than one per declaration: the reach a binding needs is the
/// same reach the load-time upgrades need (the flat graph, every layer
/// network, every nested subnet), and walking it once per parameter would make
/// applying a template's declarations quadratic in the size of the project.
fn write_parameters(document: Document, writes: &HashMap<NodeId, Vec<Parameter>>) -> Document {
    document.map_graphs(|graph| {
        graph_walk::map_subnets(graph, &|graph: &Graph| {
            let mut graph = graph.clone();
            for (node, parameters) in writes {
                if graph.node(*node).is_none() {
                    continue;
                }
                match graph.clone().set_params(*node, parameters) {
                    Ok(updated) => graph = updated,
                    // The node was there a line ago, so this cannot happen;
                    // losing a parameter write silently is the one outcome
                    // worth a log if it ever does.
                    Err(err) => {
                        tracing::warn!(%err, ?node, "an exposed parameter write was refused")
                    }
                }
            }
            graph
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::curve::KeyframeCurve;
    use crate::animation::interpolation::Interpolation;
    use crate::composition::{Composition, Layer};
    use crate::eval::{EvalContext, EvalScope, Evaluator, NodeProcessor, ResolvedParams};
    use crate::exposed::{ExposedBinding, ExposedParameter, ExposedParameters};
    use crate::graph::Node;
    use crate::id::{CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
    use crate::network::{
        CustomPortType, NET_IN_TYPE_KEY, NetworkContext, PORT_FRAME_INDEX, PORT_TIME,
    };
    use crate::types::{FrameRate, NodeData, Scalar};

    /// The node every declaration in these tests binds to.
    fn title() -> NodeId {
        NodeId::new(1)
    }

    /// The In node a port rename edits.
    fn interface() -> NodeId {
        NodeId::new(5)
    }

    /// A layer-root In node with its fixed ports, the network a custom port
    /// is added to.
    fn in_graph() -> Graph {
        Graph::new()
            .add_node(
                Node::new(interface(), NET_IN_TYPE_KEY)
                    .with_output(PORT_TIME, DataTypeId::SCALAR)
                    .with_output(PORT_FRAME_INDEX, DataTypeId::SCALAR),
            )
            .unwrap()
    }

    fn title_node() -> Node {
        Node::new(title(), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("text", ParameterValue::String("Ravel".into()))
            .with_param(
                "scale",
                ParameterValue::Channel(AnimationChannel::constant(1.0)),
            )
            .with_param("offset", ParameterValue::vec2(0.0, 0.0))
    }

    /// A document whose single layer network holds [`title_node`], plus the
    /// declarations bound to it.
    fn document(declarations: ExposedParameters) -> Document {
        let network = Graph::new().add_node(title_node()).unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::new(1), "Title", network).with_time(0, 0, 100));
        Document::default()
            .with_composition(comp)
            .with_exposed_parameters(declarations)
    }

    fn declaration(name: &str, default: ExposedValue, key: &str) -> ExposedParameter {
        ExposedParameter::inferred(name, default, ExposedBinding::new(title(), key)).unwrap()
    }

    fn declarations(entries: impl IntoIterator<Item = ExposedParameter>) -> ExposedParameters {
        ExposedParameters::from_declarations(entries).unwrap()
    }

    fn headline_document() -> Document {
        document(declarations([declaration(
            "headline",
            ExposedValue::String("Ravel".into()),
            "text",
        )]))
    }

    fn given(pairs: [(&str, ExposedValue); 1]) -> HashMap<String, ExposedValue> {
        pairs
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }

    /// The parameter `key` on the bound node, wherever the document keeps it.
    fn parameter(document: &Document, key: &str) -> ParameterValue {
        find_node(document, title())
            .expect("the bound node is in the document")
            .parameters
            .iter()
            .find(|parameter| parameter.key == key)
            .expect("the parameter is on the node")
            .value
            .clone()
    }

    /// Rebuild the document with `network` as its only layer's network.
    fn with_network(document: &Document, network: Graph) -> Document {
        let mut next = document.clone();
        let id = *next.compositions.keys().next().unwrap();
        let mut comp = (**next.compositions.get(&id).unwrap()).clone();
        comp.layers[0].network = network;
        next.compositions.insert(id, Arc::new(comp));
        next
    }

    fn network_of(document: &Document) -> Graph {
        document
            .compositions
            .values()
            .next()
            .unwrap()
            .layers
            .head()
            .unwrap()
            .network
            .clone()
    }

    // ---- the value reaches the parameter ----------------------------------

    #[test]
    fn a_value_reaches_the_bound_parameter() {
        let applied = apply(
            headline_document(),
            &given([("headline", ExposedValue::String("Hello".into()))]),
            AssetContext::default(),
        )
        .expect("the value matches the declaration");
        assert!(applied.issues.is_empty(), "{:?}", applied.issues);
        assert_eq!(
            parameter(&applied.document, "text"),
            ParameterValue::String("Hello".into())
        );
    }

    /// The declaration is a name, so the same call has to work when the
    /// binding sits inside a subnet the caller has never heard of.
    #[test]
    fn a_binding_inside_a_subnet_is_reached() {
        let inner = Graph::new().add_node(title_node()).unwrap();
        let outer = Graph::new()
            .add_node(Node::new(NodeId::new(9), "subnet").with_subnet(inner))
            .unwrap();
        let document = with_network(&headline_document(), outer);

        let applied = apply(
            document,
            &given([("headline", ExposedValue::String("Nested".into()))]),
            AssetContext::default(),
        )
        .unwrap();
        assert!(applied.issues.is_empty(), "{:?}", applied.issues);
        assert_eq!(
            parameter(&applied.document, "text"),
            ParameterValue::String("Nested".into())
        );
    }

    #[test]
    fn a_channel_parameter_takes_a_float() {
        let document = document(declarations([declaration(
            "scale",
            ExposedValue::Float(1.0),
            "scale",
        )]));
        let applied = apply(
            document,
            &given([("scale", ExposedValue::Float(4.0))]),
            AssetContext::default(),
        )
        .unwrap();
        assert_eq!(
            parameter(&applied.document, "scale"),
            ParameterValue::Channel(AnimationChannel::constant(4.0))
        );
    }

    // ---- robustness against editing the network ---------------------------

    /// Renaming the node — the label a user types — must not be able to break
    /// an external contract: the binding is a node id, and a label is not part
    /// of it.
    #[test]
    fn renaming_the_bound_node_keeps_the_declaration_working() {
        let document = headline_document();
        let mut renamed = title_node();
        renamed.metadata.label = Some("Headline card".to_string());
        let document = with_network(
            &document,
            network_of(&document).replace_node(Arc::new(renamed)),
        );

        let applied = apply(
            document,
            &given([("headline", ExposedValue::String("Hello".into()))]),
            AssetContext::default(),
        )
        .unwrap();
        assert!(applied.issues.is_empty(), "{:?}", applied.issues);
        assert_eq!(
            parameter(&applied.document, "text"),
            ParameterValue::String("Hello".into())
        );
    }

    /// Rewiring moves edges, not parameters. The binding survives both a new
    /// edge into the bound node and the removal of one.
    #[test]
    fn rewiring_the_bound_node_keeps_the_declaration_working() {
        let document = headline_document();
        let source = Node::new(NodeId::new(2), "test").with_output("out", DataTypeId::SCALAR);
        let bound = title_node().with_input("in", &[DataTypeId::SCALAR]);
        let network = network_of(&document)
            .replace_node(Arc::new(bound))
            .add_node(source)
            .unwrap();
        let edge = EdgeId::next();
        let network = network
            .add_edge(
                edge,
                NodeId::new(2),
                OutputPortIndex(0),
                title(),
                InputPortIndex(0),
            )
            .unwrap();
        let network = network.remove_edge(edge).unwrap();
        let document = with_network(&document, network);

        let applied = apply(
            document,
            &given([("headline", ExposedValue::String("Hello".into()))]),
            AssetContext::default(),
        )
        .unwrap();
        assert!(applied.issues.is_empty(), "{:?}", applied.issues);
        assert_eq!(
            parameter(&applied.document, "text"),
            ParameterValue::String("Hello".into())
        );
    }

    /// The fifth place a custom-port rename has to reach. The rename reports
    /// the key it moved and the document commit carries it into the
    /// declarations, so the contract is untouched by an edit to the interface
    /// behind it.
    #[test]
    fn renaming_the_bound_port_carries_the_declaration_with_it() {
        let network = crate::network::add_custom_port(
            in_graph(),
            interface(),
            "headline",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .expect("a float port is allowed at a layer root");

        let document = document(declarations([ExposedParameter::inferred(
            "headline",
            ExposedValue::Float(0.0),
            ExposedBinding::new(interface(), "headline"),
        )
        .unwrap()]));
        let document = with_network(&document, network);

        let renamed = crate::network::rename_custom_port(
            network_of(&document),
            interface(),
            "headline",
            "title",
            NetworkContext::LayerRoot,
        )
        .expect("the port is custom");
        let rename = renamed.key_rename().cloned().expect("the parameter moved");
        let document = with_network(&document, renamed.into_graph());
        let document = follow_key_rename(document, &rename);

        assert!(resolve(&document).is_empty(), "{:?}", resolve(&document));
        let applied = apply(
            document,
            &given([("headline", ExposedValue::Float(3.0))]),
            AssetContext::default(),
        )
        .unwrap();
        assert!(applied.issues.is_empty(), "{:?}", applied.issues);
        let node = find_node(&applied.document, interface()).unwrap();
        let parameter = node
            .parameters
            .iter()
            .find(|parameter| parameter.key == "title")
            .expect("the parameter moved with the port");
        assert_eq!(
            parameter.value,
            ParameterValue::Channel(AnimationChannel::constant(3.0))
        );
    }

    /// Without the follow-through the same rename orphans the declaration.
    /// This is the partial application the design forbids, pinned so it
    /// cannot come back as "the rename works, the contract silently doesn't".
    #[test]
    fn a_rename_that_is_not_followed_leaves_the_declaration_unresolved() {
        let network = crate::network::add_custom_port(
            in_graph(),
            interface(),
            "headline",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let document = document(declarations([ExposedParameter::inferred(
            "headline",
            ExposedValue::Float(0.0),
            ExposedBinding::new(interface(), "headline"),
        )
        .unwrap()]));
        let document = with_network(&document, network);
        let renamed = crate::network::rename_custom_port(
            network_of(&document),
            interface(),
            "headline",
            "title",
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let document = with_network(&document, renamed.into_graph());

        assert_eq!(
            resolve(&document)
                .into_iter()
                .map(|issue| issue.reason)
                .collect::<Vec<_>>(),
            [BindingIssueReason::ParameterMissing]
        );
    }

    // ---- a binding that no longer lands -----------------------------------

    /// Echoes a parameter so the evaluator has something to do.
    struct ParamEcho;
    impl NodeProcessor for ParamEcho {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(Scalar(params.f32_or("scale", -1.0))))
        }
    }

    #[test]
    fn a_deleted_binding_target_is_reported_and_evaluation_still_runs() {
        let document = document(declarations([
            declaration("headline", ExposedValue::String("Ravel".into()), "text"),
            declaration("scale", ExposedValue::Float(1.0), "scale"),
        ]));
        // The whole node the declarations bind to is gone.
        let document = with_network(&document, Graph::new().add_node(surviving_node()).unwrap());

        let applied = apply(
            document,
            &given([("headline", ExposedValue::String("Hello".into()))]),
            AssetContext::default(),
        )
        .expect("a broken binding is not a caller error");
        assert_eq!(
            applied.issues,
            [BindingIssue {
                name: "headline".to_string(),
                node: title(),
                key: "text".to_string(),
                reason: BindingIssueReason::NodeMissing,
            }]
        );
        // Both declarations survive: the contract is not narrowed by the
        // document losing the parameter behind it.
        assert_eq!(applied.document.exposed_parameters.len(), 2);
        assert_eq!(resolve(&applied.document).len(), 2);

        // And the document still evaluates.
        let network = network_of(&applied.document);
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(2), Arc::new(ParamEcho));
        let value = evaluator
            .evaluate(
                &network,
                NodeId::new(2),
                &EvalContext::new(0, FrameRate::new(30, 1), (16, 16)),
            )
            .expect("evaluation is unaffected by an unresolved declaration");
        assert_eq!(value.as_any().downcast_ref::<Scalar>().unwrap().0, 2.0);
    }

    fn surviving_node() -> Node {
        Node::new(NodeId::new(2), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param(
                "scale",
                ParameterValue::Channel(AnimationChannel::constant(2.0)),
            )
    }

    #[test]
    fn a_binding_to_a_missing_parameter_is_reported() {
        let document = document(declarations([declaration(
            "headline",
            ExposedValue::String("Ravel".into()),
            "no_such_key",
        )]));
        let issues = resolve(&document);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].reason, BindingIssueReason::ParameterMissing);
    }

    /// Retyping the parameter behind a declaration leaves the declaration
    /// naming something it cannot drive. Reported, not written.
    #[test]
    fn a_binding_to_an_incompatible_parameter_is_reported() {
        let document = document(declarations([declaration(
            "headline",
            ExposedValue::String("Ravel".into()),
            "scale",
        )]));
        let issues = resolve(&document);
        assert_eq!(
            issues[0].reason,
            BindingIssueReason::KindMismatch {
                declared: ExposedType::String,
                parameter_kind: "channel",
            }
        );

        let applied = apply(
            document,
            &given([("headline", ExposedValue::String("Hello".into()))]),
            AssetContext::default(),
        )
        .unwrap();
        assert_eq!(applied.issues.len(), 1);
        assert_eq!(
            parameter(&applied.document, "scale"),
            ParameterValue::Channel(AnimationChannel::constant(1.0)),
            "nothing was written"
        );
    }

    // ---- the call is validated before anything is written -----------------

    #[test]
    fn a_value_of_the_wrong_type_is_rejected_before_anything_is_written() {
        let document = document(declarations([
            declaration("headline", ExposedValue::String("Ravel".into()), "text"),
            declaration("scale", ExposedValue::Float(1.0), "scale"),
        ]));
        let values: HashMap<String, ExposedValue> = [
            ("headline".to_string(), ExposedValue::String("Hello".into())),
            ("scale".to_string(), ExposedValue::Bool(true)),
        ]
        .into_iter()
        .collect();

        let err = apply(document.clone(), &values, AssetContext::default())
            .expect_err("a bool is not a float");
        assert_eq!(
            err,
            ExposedApplyError::TypeMismatch {
                name: "scale".to_string(),
                declared: ExposedType::Float,
                found: ExposedType::Bool,
            }
        );
        // The valid half of the same call did not land either.
        assert_eq!(
            parameter(&document, "text"),
            ParameterValue::String("Ravel".into())
        );
    }

    #[test]
    fn an_undeclared_name_is_rejected() {
        let err = apply(
            headline_document(),
            &given([("subtitle", ExposedValue::String("Hello".into()))]),
            AssetContext::default(),
        )
        .expect_err("nothing declares that name");
        assert_eq!(err, ExposedApplyError::Undeclared("subtitle".to_string()));
    }

    #[test]
    fn a_non_finite_value_is_rejected() {
        let document = document(declarations([declaration(
            "scale",
            ExposedValue::Float(1.0),
            "scale",
        )]));
        let err = apply(
            document,
            &given([("scale", ExposedValue::Float(f32::NAN))]),
            AssetContext::default(),
        )
        .expect_err("a NaN is not a value a contract can carry");
        assert_eq!(err, ExposedApplyError::NonFiniteValue("scale".to_string()));
    }

    // ---- sources other than a constant ------------------------------------

    fn keyframed() -> AnimationChannel {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 1.0, Interpolation::Linear);
        curve.insert(30, 5.0, Interpolation::Linear);
        AnimationChannel::keyframes(curve)
    }

    /// The decision this module exists to make: a declaration gives a default,
    /// it does not replace an animation. Rendering a template must not delete
    /// the keyframes on the parameter it sets.
    #[test]
    fn a_keyframed_parameter_keeps_its_keyframes_and_is_reported() {
        let document = document(declarations([declaration(
            "scale",
            ExposedValue::Float(1.0),
            "scale",
        )]));
        let mut node = title_node();
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == "scale")
            .unwrap()
            .value = ParameterValue::Channel(keyframed());
        let document = with_network(
            &document,
            network_of(&document).replace_node(Arc::new(node)),
        );

        let applied = apply(
            document,
            &given([("scale", ExposedValue::Float(4.0))]),
            AssetContext::default(),
        )
        .unwrap();
        assert_eq!(
            applied.issues[0].reason,
            BindingIssueReason::AnimatedComponents {
                components: vec![0]
            }
        );
        assert_eq!(
            parameter(&applied.document, "scale"),
            ParameterValue::Channel(keyframed()),
            "the keyframes are exactly what they were"
        );
    }

    /// Per component: the animated half of a vector keeps its animation, the
    /// constant half takes the value — and the caller is told which half did
    /// not land. Reporting is the whole point: a partial write that called
    /// itself applied would show up in a listing as `resolved` while half the
    /// components ran on the document's own values.
    #[test]
    fn a_partly_animated_vector_takes_the_value_on_its_constant_components() {
        let document = document(declarations([declaration(
            "offset",
            ExposedValue::Vec2(Vec2(0.0, 0.0)),
            "offset",
        )]));
        let mut node = title_node();
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == "offset")
            .unwrap()
            .value = ParameterValue::Channel2([keyframed(), AnimationChannel::constant(0.0)]);
        let document = with_network(
            &document,
            network_of(&document).replace_node(Arc::new(node)),
        );

        let applied = apply(
            document,
            &given([("offset", ExposedValue::Vec2(Vec2(7.0, 9.0)))]),
            AssetContext::default(),
        )
        .unwrap();
        assert_eq!(
            applied.issues,
            [BindingIssue {
                name: "offset".to_string(),
                node: title(),
                key: "offset".to_string(),
                reason: BindingIssueReason::AnimatedComponents {
                    components: vec![0]
                },
            }],
            "the component that kept its animation is reported"
        );
        assert_eq!(
            parameter(&applied.document, "offset"),
            ParameterValue::Channel2([keyframed(), AnimationChannel::constant(9.0)]),
            "and the constant component still took the value"
        );
    }

    /// The same partial application seen through the contract check: a
    /// declaration that only half lands is not a resolved one.
    #[test]
    fn a_partly_animated_binding_is_reported_by_resolve() {
        let document = document(declarations([declaration(
            "offset",
            ExposedValue::Vec2(Vec2(0.0, 0.0)),
            "offset",
        )]));
        let mut node = title_node();
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == "offset")
            .unwrap()
            .value = ParameterValue::Channel2([keyframed(), AnimationChannel::constant(0.0)]);
        let document = with_network(
            &document,
            network_of(&document).replace_node(Arc::new(node)),
        );

        assert_eq!(
            resolve(&document)
                .into_iter()
                .map(|issue| issue.reason)
                .collect::<Vec<_>>(),
            [BindingIssueReason::AnimatedComponents {
                components: vec![0]
            }]
        );
    }

    /// An expression is not a constant either, and the same rule holds: the
    /// source stays, the value does not land.
    #[test]
    fn an_expression_source_is_left_alone() {
        let document = document(declarations([declaration(
            "scale",
            ExposedValue::Float(1.0),
            "scale",
        )]));
        let expression = AnimationChannel::new(ChannelSource::Expression(
            crate::animation::channel::ParameterExpression::new("time * 2"),
        ));
        let mut node = title_node();
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == "scale")
            .unwrap()
            .value = ParameterValue::Channel(expression.clone());
        let document = with_network(
            &document,
            network_of(&document).replace_node(Arc::new(node)),
        );

        let applied = apply(
            document,
            &given([("scale", ExposedValue::Float(4.0))]),
            AssetContext::default(),
        )
        .unwrap();
        assert_eq!(
            applied.issues[0].reason,
            BindingIssueReason::AnimatedComponents {
                components: vec![0]
            }
        );
        assert_eq!(
            parameter(&applied.document, "scale"),
            ParameterValue::Channel(expression)
        );
    }

    // ---- media references (REQ-PROJ-005) ----------------------------------

    fn media_node() -> NodeId {
        NodeId::new(3)
    }

    /// The asset `media_document`'s node starts out referencing. A fixed id so
    /// a test can name both halves of the reference.
    fn original_asset() -> AssetId {
        AssetId::new(1)
    }

    /// A document whose layer network holds a media node referencing the asset
    /// named `original`, with `plate` declared against its asset reference.
    fn media_document() -> Document {
        let network = Graph::new()
            .add_node(
                Node::new(media_node(), "media")
                    .with_output("frame", DataTypeId::FRAME_BUFFER)
                    .with_param(
                        "asset_id",
                        ParameterValue::String(original_asset().to_param_value()),
                    ),
            )
            .unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::new(1), "Plate", network).with_time(0, 0, 100));
        Document::default()
            .with_composition(comp)
            .with_media_asset(original_asset(), "/footage/original.mov")
            .with_exposed_parameters(declarations([ExposedParameter::inferred(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/original.mov".into())),
                ExposedBinding::new(media_node(), "asset_id"),
            )
            .unwrap()]))
    }

    /// What the media processor reads: the asset the node references, and the
    /// location the document's asset table resolves it to.
    fn media_source(document: &Document) -> (Option<AssetId>, Option<PathBuf>) {
        let ParameterValue::String(stored) = parameter_of(document, media_node(), "asset_id")
        else {
            panic!("the asset reference is a string");
        };
        let asset = AssetId::from_param_value(&stored);
        let resolved = asset
            .and_then(|id| document.get_media_asset(id))
            .and_then(|entry| entry.resolved.clone());
        (asset, resolved)
    }

    /// The display name of the asset `document`'s media node references.
    fn media_asset_name(document: &Document) -> Option<String> {
        let (asset, _) = media_source(document);
        asset
            .and_then(|id| document.get_media_asset(id))
            .map(|entry| entry.name.clone())
    }

    fn parameter_of(document: &Document, node: NodeId, key: &str) -> ParameterValue {
        find_node(document, node)
            .expect("the node is in the document")
            .parameters
            .iter()
            .find(|parameter| parameter.key == key)
            .expect("the parameter is on the node")
            .value
            .clone()
    }

    /// A project root holding `footage/replacement.mov`.
    fn project_with_footage() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("a temporary project root");
        std::fs::create_dir(root.path().join("footage")).unwrap();
        std::fs::write(
            root.path().join("footage/replacement.mov"),
            b"not really a movie",
        )
        .unwrap();
        root
    }

    /// The swap changes exactly the two things the media processor reads —
    /// which asset the node names, and where that asset is — so the next
    /// evaluation decodes a different file.
    #[test]
    fn swapping_the_media_changes_what_the_evaluation_reads() {
        let root = project_with_footage();
        let document = media_document();
        let before = media_source(&document);
        assert_eq!(before.0, Some(original_asset()));

        let applied = apply(
            document,
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .expect("the file is there");
        assert!(applied.issues.is_empty(), "{:?}", applied.issues);

        let (asset_id, resolved) = media_source(&applied.document);
        assert_eq!(
            media_asset_name(&applied.document).as_deref(),
            Some("exposed:plate"),
            "the node references the entry this declaration registered"
        );
        assert_eq!(
            resolved.as_deref(),
            Some(root.path().join("footage/replacement.mov").as_path()),
            "and the entry resolves to the file the caller named"
        );
        assert_ne!((asset_id, resolved), before);

        // The entry the node used to reference is still there: another node or
        // a layer's audio source may reference it.
        assert!(applied.document.get_media_asset(original_asset()).is_some());
    }

    /// Applying the same reference twice is the same document. The id *is*
    /// minted, so idempotence rests on the second apply finding the entry the
    /// first one registered — by the declaration recorded on it
    /// (`MediaAssetEntry::exposed_owner`) — and reusing its id instead of
    /// minting a second one beside it.
    #[test]
    fn swapping_the_same_media_twice_is_idempotent() {
        let root = project_with_footage();
        let values = given([(
            "plate",
            ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
        )]);
        let once = apply(media_document(), &values, AssetContext::rooted(root.path()))
            .unwrap()
            .document;
        let twice = apply(once.clone(), &values, AssetContext::rooted(root.path()))
            .unwrap()
            .document;
        assert_eq!(once, twice);
        // Spelled out because document equality would also hold if *both*
        // applies had grown the table by one.
        assert_eq!(
            twice.media_assets.len(),
            2,
            "the original entry and the one this declaration owns, and nothing else"
        );
    }

    /// A user may name an asset anything, `exposed:plate` included. Since
    /// ownership is recorded on the entry and never read out of its name, such
    /// an asset is not the declaration's: the swap goes to a fresh entry and
    /// leaves the impostor exactly as it was.
    #[test]
    fn an_asset_merely_named_like_the_declaration_is_not_taken_over() {
        let root = project_with_footage();
        // Minted, not a fixed number: every test in this binary shares the
        // id counter, so a hand-picked id can collide with one an `apply`
        // elsewhere mints.
        let impostor = AssetId::next();
        let document = media_document().with_media_asset_entry(impostor, {
            let mut entry = MediaAssetEntry::from_absolute("/footage/somebody_elses.mov");
            entry.name = asset_name_for("plate");
            entry
        });

        let applied = apply(
            document,
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .expect("a name is not a claim");

        let (asset, resolved) = media_source(&applied.document);
        assert_ne!(asset, Some(impostor), "the impostor was not adopted");
        assert_eq!(
            resolved.as_deref(),
            Some(root.path().join("footage/replacement.mov").as_path())
        );
        assert_eq!(
            applied
                .document
                .get_media_asset(impostor)
                .map(|entry| entry.path.clone()),
            Some(AssetPath::Absolute("/footage/somebody_elses.mov".into())),
            "and it still points where it did"
        );
    }

    /// The entry a declaration owns can stop being the one its node reads —
    /// the user pointed that node somewhere else. Overwriting it then would
    /// swap the media under whatever *does* read it, so the swap is refused.
    #[test]
    fn a_swap_whose_owned_asset_the_node_no_longer_reads_is_refused() {
        let root = project_with_footage();
        let owned = AssetId::next();
        let document = media_document().with_media_asset_entry(owned, {
            let mut entry = MediaAssetEntry::from_absolute("/footage/earlier_swap.mov");
            entry.name = asset_name_for("plate");
            entry.exposed_owner = Some("plate".to_string());
            entry
        });

        let err = apply(
            document.clone(),
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .expect_err("the node this declaration binds reads a different asset");
        assert!(matches!(
            err,
            ExposedApplyError::AssetIdTaken { ref name, ref asset, .. }
                if name == "plate" && asset == "exposed:plate"
        ));

        assert_eq!(
            document.get_media_asset(owned).map(|e| e.path.clone()),
            Some(AssetPath::Absolute("/footage/earlier_swap.mov".into())),
            "the owned entry is untouched"
        );
        assert_eq!(
            media_source(&document).0,
            Some(original_asset()),
            "and so is the node"
        );
    }

    /// Only a hand-edited document can have two entries claiming one
    /// declaration. Replacing either would be a coin toss over somebody's
    /// media, so the call is refused before anything is written.
    #[test]
    fn two_entries_claiming_one_declaration_are_refused() {
        let root = project_with_footage();
        let mut document = media_document();
        for (id, path) in [(AssetId::next(), "/a.mov"), (AssetId::next(), "/b.mov")] {
            document = document.with_media_asset_entry(id, {
                let mut entry = MediaAssetEntry::from_absolute(path);
                entry.exposed_owner = Some("plate".to_string());
                entry
            });
        }

        let err = apply(
            document,
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .expect_err("which of the two would be replaced is undecidable");
        assert!(matches!(
            err,
            ExposedApplyError::AssetOwnerAmbiguous { ref name, count }
                if name == "plate" && count == 2
        ));
    }

    /// The MediaBin can rename an asset a declaration owns (`AID-3`). The
    /// declaration owns the *file reference*, not the label, so a later apply
    /// finds the same entry, writes the new path into it, and leaves the name
    /// the user chose alone.
    #[test]
    fn a_reapply_keeps_the_name_the_user_gave_an_owned_asset() {
        let root = project_with_footage();
        std::fs::write(root.path().join("footage/second.mov"), b"another one").unwrap();
        let swap = |document, file: &str| {
            apply(
                document,
                &given([(
                    "plate",
                    ExposedValue::Media(AssetPath::Relative(format!("./footage/{file}"))),
                )]),
                AssetContext::rooted(root.path()),
            )
            .expect("the file is there")
            .document
        };

        let once = swap(media_document(), "replacement.mov");
        let owned = media_source(&once).0.expect("the node references an asset");

        // The rename the MediaBin commits: the entry, with a new name.
        let renamed = {
            let mut entry = once.get_media_asset(owned).unwrap().clone();
            entry.name = "Backdrop".to_string();
            once.with_media_asset_entry(owned, entry)
        };

        let again = swap(renamed, "second.mov");
        assert_eq!(
            media_source(&again).0,
            Some(owned),
            "the same entry was reused"
        );
        assert_eq!(
            media_asset_name(&again).as_deref(),
            Some("Backdrop"),
            "and the user's name survived the apply"
        );
        assert_eq!(
            media_source(&again).1.as_deref(),
            Some(root.path().join("footage/second.mov").as_path()),
            "while the path it points at is the declaration's to write"
        );
        assert_eq!(
            again.media_assets.len(),
            2,
            "a rename does not cost the declaration its entry"
        );
    }

    /// The failure this rule exists for: a render that starts and produces
    /// blank frames is worse than one that refuses to start.
    #[test]
    fn an_absent_asset_is_an_explicit_failure() {
        let root = project_with_footage();
        let document = media_document();
        let err = apply(
            document.clone(),
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/gone.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .expect_err("there is no such file");
        match err {
            ExposedApplyError::MediaNotFound { name, resolved, .. } => {
                assert_eq!(name, "plate");
                assert_eq!(resolved, root.path().join("footage/gone.mov"));
            }
            other => panic!("expected a missing-file error, got {other}"),
        }
        assert_eq!(
            media_source(&document).0,
            Some(original_asset()),
            "a refused call writes nothing"
        );
    }

    /// A directory is not media. It exists, so an existence check passes it
    /// through to the decoder, where the same mistake surfaces as a decode
    /// failure a long way from the argument that caused it.
    #[test]
    fn a_directory_is_not_an_asset() {
        let root = project_with_footage();
        let err = apply(
            media_document(),
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .expect_err("a directory is not a file");
        assert!(matches!(
            err,
            ExposedApplyError::MediaNotFound { ref name, .. } if name == "plate"
        ));
    }

    /// A relative reference with no project root to anchor it is not a missing
    /// file — it is a reference with no location at all, and saying so is more
    /// useful than naming a path that was never meant.
    #[test]
    fn a_reference_that_cannot_be_located_is_reported_as_such() {
        let err = apply(
            media_document(),
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::default(),
        )
        .expect_err("an unsaved project cannot anchor a relative path");
        assert!(matches!(
            err,
            ExposedApplyError::MediaUnresolved { ref name, .. } if name == "plate"
        ));
    }

    /// The three forms REQ-PROJ-005 defines all resolve: absolute, project
    /// relative, and `${VAR}`-prefixed — including the implicit
    /// `${PROJECT_ROOT}`.
    #[test]
    fn relative_and_variable_references_resolve() {
        let root = project_with_footage();
        let absolute = root.path().join("footage/replacement.mov");
        let mut vars = HashMap::new();
        vars.insert(
            "FOOTAGE".to_string(),
            root.path().join("footage").to_string_lossy().into_owned(),
        );

        let cases = [
            AssetPath::Absolute(absolute.clone()),
            AssetPath::Relative("./footage/replacement.mov".into()),
            AssetPath::Variable("${PROJECT_ROOT}/footage/replacement.mov".into()),
            AssetPath::Variable("${FOOTAGE}/replacement.mov".into()),
        ];
        for path in cases {
            let applied = apply(
                media_document(),
                &given([("plate", ExposedValue::Media(path.clone()))]),
                AssetContext::new(Some(root.path()), &vars),
            )
            .unwrap_or_else(|err| panic!("{path} should resolve: {err}"));
            let (asset_id, resolved) = media_source(&applied.document);
            assert_eq!(
                media_asset_name(&applied.document).as_deref(),
                Some("exposed:plate")
            );
            assert_eq!(resolved.as_deref(), Some(absolute.as_path()), "{path}");

            // The persisted form is the one the caller gave: a relative or
            // variable reference stays portable instead of being frozen to
            // this machine's absolute path.
            assert_eq!(
                applied
                    .document
                    .get_media_asset(asset_id.expect("the node references an asset"))
                    .unwrap()
                    .path,
                path
            );
        }
    }

    /// The extent decision: swapping in media of a different size or length
    /// changes the reference and nothing else. A template whose layout
    /// depended on the file the caller passed would not be a template.
    #[test]
    fn a_swap_does_not_compensate_for_a_different_size_or_duration() {
        let root = project_with_footage();
        let document = media_document().with_media_asset_entry(original_asset(), {
            let mut entry = MediaAssetEntry::from_absolute("/footage/original.mov");
            entry.metadata.width = Some(4096);
            entry.metadata.height = Some(2160);
            entry.metadata.duration_secs = Some(30.0);
            entry
        });

        let applied = apply(
            document.clone(),
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .unwrap();

        let comp = applied.document.compositions.values().next().unwrap();
        let before = document.compositions.values().next().unwrap();
        assert_eq!(comp.resolution, before.resolution);
        assert_eq!(comp.duration_frames, before.duration_frames);
        let layer = comp.layers.head().unwrap();
        let layer_before = before.layers.head().unwrap();
        assert_eq!(
            (layer.start_frame, layer.in_frame, layer.out_frame),
            (
                layer_before.start_frame,
                layer_before.in_frame,
                layer_before.out_frame
            ),
        );
        assert_eq!(layer.transform, layer_before.transform);

        // Nor is the replaced asset's metadata carried over: describing the
        // new file needs a decoder, which the core layer does not have.
        let entry = applied
            .document
            .get_media_asset(media_source(&applied.document).0.expect("a reference"))
            .expect("the new entry");
        assert_eq!(entry.metadata, Default::default());
        assert_eq!(entry.kind, AssetKind::Container);
    }

    /// A media node can carry other string parameters. Binding a media
    /// declaration to one of those is refused: writing an asset id there would
    /// report a swap the processor never sees — the picture would not change,
    /// and the parameter that did change would hold an asset id.
    #[test]
    fn a_media_declaration_bound_to_another_parameter_is_reported() {
        let root = project_with_footage();
        let network = Graph::new()
            .add_node(
                Node::new(media_node(), "media")
                    .with_output("frame", DataTypeId::FRAME_BUFFER)
                    .with_param(
                        "asset_id",
                        ParameterValue::String(original_asset().to_param_value()),
                    )
                    // Any other string parameter a media node may carry.
                    .with_param("label", ParameterValue::String("Plate A".into())),
            )
            .unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::new(1), "Plate", network).with_time(0, 0, 100));
        let document = Document::default()
            .with_composition(comp)
            .with_media_asset(original_asset(), "/footage/original.mov")
            .with_exposed_parameters(declarations([ExposedParameter::inferred(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/original.mov".into())),
                ExposedBinding::new(media_node(), "label"),
            )
            .unwrap()]));

        assert_eq!(
            resolve(&document)
                .into_iter()
                .map(|issue| issue.reason)
                .collect::<Vec<_>>(),
            [BindingIssueReason::NotAnAssetReference {
                expected: "asset_id"
            }],
            "a string parameter that is not the asset reference is not a binding"
        );

        let applied = apply(
            document,
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .unwrap();
        assert_eq!(applied.issues.len(), 1);
        assert_eq!(
            parameter_of(&applied.document, media_node(), "label"),
            ParameterValue::String("Plate A".into()),
            "the parameter it was wrongly bound to is untouched"
        );
        assert_eq!(
            media_source(&applied.document).0,
            Some(original_asset()),
            "and the asset the node actually reads is unchanged"
        );
        assert_eq!(
            applied.document.media_asset_id_by_name("exposed:plate"),
            None
        );
    }

    /// A media declaration bound to something that is not a media node writes
    /// nothing: an asset id stored in a text node's `text` would corrupt that
    /// parameter and swap no media at all.
    #[test]
    fn a_media_declaration_bound_to_another_node_is_reported() {
        let root = project_with_footage();
        let document = document(declarations([ExposedParameter::inferred(
            "plate",
            ExposedValue::Media(AssetPath::Relative("./footage/original.mov".into())),
            ExposedBinding::new(title(), "text"),
        )
        .unwrap()]));

        assert_eq!(
            resolve(&document)
                .into_iter()
                .map(|issue| issue.reason)
                .collect::<Vec<_>>(),
            [BindingIssueReason::NotAMediaNode {
                type_key: "test".to_string()
            }]
        );

        let applied = apply(
            document,
            &given([(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/replacement.mov".into())),
            )]),
            AssetContext::rooted(root.path()),
        )
        .unwrap();
        assert_eq!(applied.issues.len(), 1);
        assert_eq!(
            parameter(&applied.document, "text"),
            ParameterValue::String("Ravel".into()),
            "the text parameter is untouched"
        );
        assert_eq!(
            applied.document.media_asset_id_by_name("exposed:plate"),
            None,
            "and no asset was registered for a swap that did not happen"
        );
    }

    /// `resolve` answers the binding question without a file in hand, so a
    /// sound media binding is not reported just because nothing resolved it.
    #[test]
    fn a_sound_media_binding_resolves_without_a_location() {
        assert!(resolve(&media_document()).is_empty());
    }

    // ---- nothing supplied -------------------------------------------------

    /// Applying nothing is applying nothing: the defaults are a listing a
    /// caller reads, not a reset the document has to take.
    #[test]
    fn declarations_that_were_not_given_a_value_are_left_alone() {
        let document = document(declarations([declaration(
            "headline",
            ExposedValue::String("A different default".into()),
            "text",
        )]));
        let applied = apply(document.clone(), &HashMap::new(), AssetContext::default()).unwrap();
        assert_eq!(applied.document, document);
    }

    // ---- seeding a declaration from a parameter (EXPO-5) ------------------

    fn seed(document: &Document, node: NodeId, key: &str) -> Option<ExposedValue> {
        seed_value(document, &ExposedBinding::new(node, key), 0)
    }

    #[test]
    fn a_parameter_seeds_the_constant_it_holds() {
        let document = document(ExposedParameters::new());
        assert_eq!(
            seed(&document, title(), "text"),
            Some(ExposedValue::String("Ravel".into()))
        );
        assert_eq!(
            seed(&document, title(), "scale"),
            Some(ExposedValue::Float(1.0))
        );
        assert_eq!(
            seed(&document, title(), "offset"),
            Some(ExposedValue::Vec2(Vec2(0.0, 0.0)))
        );
    }

    /// [`title_node`] with `key` holding `value` instead.
    fn with_parameter(document: &Document, key: &str, value: ParameterValue) -> Document {
        let mut node = title_node();
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == key)
            .expect("the key is one title_node declares")
            .value = value;
        with_network(document, Graph::new().add_node(node).unwrap())
    }

    /// The property that makes this the right place for the mapping: a
    /// declaration seeded from a parameter always writes back to it. If the
    /// panel invented its own pairing it could mint a declaration `apply`
    /// refuses, and the user would see a contract that never resolves.
    #[test]
    fn a_seeded_declaration_resolves_against_the_parameter_it_came_from() {
        let node = Node::new(title(), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("count", ParameterValue::Int(7))
            .with_param("on", ParameterValue::Bool(true))
            .with_param("depth", ParameterValue::Float(2.5))
            .with_param(
                "triple",
                ParameterValue::Channel3([
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(2.0),
                    AnimationChannel::constant(3.0),
                ]),
            )
            .with_param(
                "tint",
                ParameterValue::Channel4([
                    AnimationChannel::constant(0.1),
                    AnimationChannel::constant(0.2),
                    AnimationChannel::constant(0.3),
                    AnimationChannel::constant(1.0),
                ]),
            );
        let base = with_network(
            &document(ExposedParameters::new()),
            Graph::new().add_node(node).unwrap(),
        );

        let mut set = ExposedParameters::new();
        for key in ["count", "on", "depth", "triple", "tint"] {
            let binding = ExposedBinding::new(title(), key);
            let value = seed_value(&base, &binding, 0).expect("every kind here is exposable");
            set.insert(
                ExposedParameter::inferred(key, value, binding).expect("the seed is finite"),
            )
            .expect("the keys differ");
        }
        assert_eq!(
            set.get("tint").map(ExposedParameter::value_type),
            Some(ExposedType::Color),
            "a four-channel parameter is presented as a colour, so it is declared as one"
        );

        let document = base.with_exposed_parameters(set);
        assert_eq!(
            resolve(&document),
            Vec::new(),
            "a declaration seeded from a parameter binds to it cleanly"
        );
    }

    /// An animated component has no constant to read, but it does have a
    /// value: the one the render produces at the frame the user is looking at.
    /// Seeding `0.0` instead would make the declaration's default — what a
    /// caller gets when they omit `--param` — a number nothing in the document
    /// ever chose.
    ///
    /// The declaration is still allowed: `resolve` is what tells the user it
    /// will not drive that component.
    #[test]
    fn an_animated_component_seeds_its_value_at_the_frame() {
        let document = with_parameter(
            &document(ExposedParameters::new()),
            "scale",
            ParameterValue::Channel(keyframed()),
        );
        let binding = ExposedBinding::new(title(), "scale");
        // `keyframed` runs 1.0 at frame 0 to 5.0 at frame 30, linearly.
        assert_eq!(
            seed_value(&document, &binding, 0),
            Some(ExposedValue::Float(1.0))
        );
        assert_eq!(
            seed_value(&document, &binding, 15),
            Some(ExposedValue::Float(3.0)),
            "the seed follows the playhead, not the start of the curve"
        );

        let value = seed_value(&document, &binding, 0).unwrap();
        let document =
            document.with_exposed_parameters(declarations([ExposedParameter::inferred(
                "scale", value, binding,
            )
            .unwrap()]));
        assert_eq!(
            resolve(&document)
                .into_iter()
                .map(|issue| issue.reason)
                .collect::<Vec<_>>(),
            [BindingIssueReason::AnimatedComponents {
                components: vec![0]
            }]
        );
    }

    /// Re-typing a declared parameter must not break its declaration. The
    /// keyframe toggle turns an `Int` into an `IntChannel` and a `String` into
    /// `StringSteps` under a declaration that already exists, and the binding
    /// still names the same node and key — so the seed keeps answering and
    /// `resolve` keeps reporting the binding, animated or not.
    ///
    /// The two differ in what a *write* can do, and that difference is the
    /// point: an `IntChannel` whose source is still a constant takes the
    /// value, while a step curve has no constant half to write into and is
    /// reported animated instead. Both are declarations that resolve; neither
    /// is a declaration that vanished.
    #[test]
    fn re_typing_a_declared_parameter_keeps_its_declaration_working() {
        let base = document(ExposedParameters::new());

        // Int -> IntChannel, still constant: seeds the same number and the
        // declaration drives it.
        let animated_int = with_network(
            &base,
            Graph::new()
                .add_node(title_node().with_param(
                    "count",
                    ParameterValue::IntChannel(AnimationChannel::constant(7.0)),
                ))
                .unwrap(),
        );
        let binding = ExposedBinding::new(title(), "count");
        assert_eq!(
            seed_value(&animated_int, &binding, 0),
            Some(ExposedValue::Int(7))
        );
        let document =
            animated_int.with_exposed_parameters(declarations([ExposedParameter::inferred(
                "count",
                ExposedValue::Int(7),
                binding,
            )
            .unwrap()]));
        assert_eq!(
            resolve(&document),
            Vec::new(),
            "a constant int channel still takes the declared value"
        );

        // String -> StringSteps: seeds the string this frame holds, and the
        // declaration is reported animated rather than silently dropped.
        let mut steps = crate::animation::StepCurve::new("Ravel".to_string());
        steps.insert(0, "Ravel".to_string());
        steps.insert(30, "Later".to_string());
        let animated_string = with_parameter(&base, "text", ParameterValue::StringSteps(steps));
        let binding = ExposedBinding::new(title(), "text");
        assert_eq!(
            seed_value(&animated_string, &binding, 0),
            Some(ExposedValue::String("Ravel".into()))
        );
        assert_eq!(
            seed_value(&animated_string, &binding, 30),
            Some(ExposedValue::String("Later".into())),
            "the seed follows the playhead"
        );
        let document =
            animated_string.with_exposed_parameters(declarations([ExposedParameter::inferred(
                "text",
                ExposedValue::String("Ravel".into()),
                binding,
            )
            .unwrap()]));
        assert_eq!(
            resolve(&document)
                .into_iter()
                .map(|issue| issue.reason)
                .collect::<Vec<_>>(),
            [BindingIssueReason::AnimatedComponents {
                components: vec![0]
            }],
            "the binding still resolves; it just cannot overwrite the keys"
        );
    }

    #[test]
    fn a_media_reference_seeds_the_path_the_asset_table_holds() {
        let document = media_document();
        assert_eq!(
            seed(&document, media_node(), "asset_id"),
            Some(ExposedValue::Media(AssetPath::Absolute(
                "/footage/original.mov".into()
            )))
        );
    }

    #[test]
    fn nothing_that_cannot_be_an_external_contract_seeds_a_value() {
        let base = document(ExposedParameters::new());
        assert_eq!(seed(&base, title(), "absent"), None);
        assert_eq!(seed(&base, NodeId::new(404), "text"), None);

        let with_path = with_parameter(&base, "text", ParameterValue::PathPoints(Vec::new()));
        assert_eq!(seed(&with_path, title(), "text"), None);

        // A media node whose asset_id names nothing the document holds has no
        // path to default to, so there is no contract to declare yet.
        let orphan = with_network(
            &media_document(),
            Graph::new()
                .add_node(
                    Node::new(media_node(), "media")
                        .with_output("frame", DataTypeId::FRAME_BUFFER)
                        .with_param("asset_id", ParameterValue::String("never imported".into())),
                )
                .unwrap(),
        );
        assert_eq!(seed(&orphan, media_node(), "asset_id"), None);
    }
}

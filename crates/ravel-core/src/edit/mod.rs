// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed, serializable edits of a [`Document`], applied without a UI.
//!
//! [`apply`] and [`apply_batch`] take a `&Document` and return a new one, so a
//! refused operation cannot have changed anything. Every result has passed
//! [`Document::validate`]. This layer does not commit: the caller owns undo
//! and evaluation, and one [`apply_batch`] result committed once is one undo
//! step.

pub mod document;
mod error;
mod ops;

use crate::composition::Document;
use crate::composition::templates::builtin_layer_template;
use crate::id::{CompId, LayerId};
use crate::registry::NodeRegistry;
pub use document::{CompositionSettings, NetworkPath};
use document::{add_composition, add_layer_from_template, remove_composition, remove_layer};
pub use error::{BatchError, EditError};
pub use ops::{CompRef, EditOp, LayerRef};
use std::collections::BTreeMap;

/// What an operation needs besides the document.
pub struct EditContext<'a> {
    pub registry: &'a NodeRegistry,
    /// The frame animated parameters are keyed at. Unused until parameter
    /// writes exist.
    pub local_frame: Option<u64>,
}

/// Something an operation created and gave an id to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Created {
    Composition(CompId),
    Layer(LayerId),
}

/// The result of an accepted edit.
#[derive(Clone, Debug)]
pub struct Applied {
    pub document: Document,
    /// Everything created, in operation order.
    pub created: Vec<Created>,
    /// The `as` aliases the batch defined and what they resolved to.
    pub aliases: BTreeMap<String, Created>,
}

type Aliases = BTreeMap<String, Created>;

/// Apply one operation. On `Err` the caller's `doc` is unchanged.
pub fn apply(doc: &Document, op: &EditOp, cx: &EditContext) -> Result<Applied, EditError> {
    apply_batch(doc, std::slice::from_ref(op), cx).map_err(|e| e.error)
}

/// Apply `ops` in order to a working copy: all of them, or none. On `Err` the
/// index of the failing operation is reported and `doc` is unchanged.
pub fn apply_batch(
    doc: &Document,
    ops: &[EditOp],
    cx: &EditContext,
) -> Result<Applied, BatchError> {
    let mut document = doc.clone();
    let mut created = Vec::new();
    let mut aliases = Aliases::new();
    for (index, op) in ops.iter().enumerate() {
        let fail = |error| BatchError { index, error };
        let made = step(&mut document, op, cx, &mut aliases).map_err(fail)?;
        document.validate().map_err(|e| fail(e.into()))?;
        created.extend(made);
    }
    Ok(Applied {
        document,
        created,
        aliases,
    })
}

/// Refuse a taken alias before the operation does any work.
fn check_alias(aliases: &Aliases, alias: &Option<String>) -> Result<(), EditError> {
    match alias {
        Some(name) if aliases.contains_key(name) => Err(EditError::DuplicateAlias(name.clone())),
        _ => Ok(()),
    }
}

fn define_alias(aliases: &mut Aliases, alias: &Option<String>, made: Created) {
    if let Some(name) = alias {
        aliases.insert(name.clone(), made);
    }
}

fn resolve_comp(aliases: &Aliases, r: &CompRef) -> Result<CompId, EditError> {
    match r {
        CompRef::Id(id) => Ok(*id),
        CompRef::Alias(name) => match aliases.get(name) {
            Some(Created::Composition(id)) => Ok(*id),
            Some(_) => Err(EditError::AliasKindMismatch(name.clone())),
            None => Err(EditError::UnknownAlias(name.clone())),
        },
    }
}

fn resolve_layer(aliases: &Aliases, r: &LayerRef) -> Result<LayerId, EditError> {
    match r {
        LayerRef::Id(id) => Ok(*id),
        LayerRef::Alias(name) => match aliases.get(name) {
            Some(Created::Layer(id)) => Ok(*id),
            Some(_) => Err(EditError::AliasKindMismatch(name.clone())),
            None => Err(EditError::UnknownAlias(name.clone())),
        },
    }
}

/// Run one operation against the working `doc`. May leave `doc` half-edited on
/// `Err`; [`apply_batch`] discards it.
fn step(
    doc: &mut Document,
    op: &EditOp,
    cx: &EditContext,
    aliases: &mut Aliases,
) -> Result<Vec<Created>, EditError> {
    match op {
        EditOp::AddComposition { settings, alias } => {
            check_alias(aliases, alias)?;
            let (next, id) = add_composition(doc, settings.clone());
            *doc = next;
            let made = Created::Composition(id);
            define_alias(aliases, alias, made);
            Ok(vec![made])
        }
        EditOp::RemoveComposition { comp } => {
            let comp = resolve_comp(aliases, comp)?;
            *doc = remove_composition(doc, comp).ok_or(EditError::CompositionNotFound(comp))?;
            Ok(Vec::new())
        }
        EditOp::AddLayer {
            comp,
            template,
            alias,
        } => {
            check_alias(aliases, alias)?;
            let comp = resolve_comp(aliases, comp)?;
            if doc.get_composition(comp).is_none() {
                return Err(EditError::CompositionNotFound(comp));
            }
            let template = builtin_layer_template(template)
                .ok_or_else(|| EditError::UnknownTemplate(template.clone()))?;
            let (next, id) = add_layer_from_template(doc, comp, template, cx.registry)?
                .ok_or(EditError::CompositionNotFound(comp))?;
            *doc = next;
            let made = Created::Layer(id);
            define_alias(aliases, alias, made);
            Ok(vec![made])
        }
        EditOp::RemoveLayer { comp, layers } => {
            let comp = resolve_comp(aliases, comp)?;
            for layer in layers {
                let layer = resolve_layer(aliases, layer)?;
                let composition = doc
                    .get_composition(comp)
                    .ok_or(EditError::CompositionNotFound(comp))?;
                let found = composition
                    .get_layer(layer)
                    .ok_or(EditError::LayerNotFound { comp, layer })?;
                if found.locked {
                    return Err(EditError::LayerLocked { comp, layer });
                }
                *doc =
                    remove_layer(doc, comp, layer).ok_or(EditError::CompositionNotFound(comp))?;
            }
            Ok(Vec::new())
        }
    }
}

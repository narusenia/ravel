// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Holding a removal until the user decides.
//!
//! A request that removes something is not refused and not applied: the door
//! emits [`super::LiveEditEvent::ConfirmRemoval`], the workspace asks in a
//! dialog, and the answer comes back through the [`RemovalRequest`] it was
//! handed. **Anything but an explicit approval is a denial** — Escape, the
//! close button, a second dialog already open, no window subscribed at all —
//! because the verdict is a one-shot sender and a dropped sender reads as
//! "no". That makes the safe answer the one nobody has to remember to give.

use std::cell::RefCell;
use std::rc::Rc;

use futures::channel::oneshot;
use ravel_core::composition::Document;
use ravel_core::edit::{CompRef, EditOp, LayerRef, NetworkPath, NetworkRef, NodeRef};
use ravel_i18n::t;

/// What the user is asked about. Cheap to clone: the dialog's builder runs
/// again on every render and each run takes its own handle.
#[derive(Clone)]
pub struct RemovalRequest {
    /// What would go, in the order the batch removes it.
    pub items: Vec<Removal>,
    verdict: Rc<RefCell<Option<oneshot::Sender<bool>>>>,
}

impl RemovalRequest {
    pub(super) fn new(items: Vec<Removal>) -> (Self, oneshot::Receiver<bool>) {
        let (tx, rx) = oneshot::channel();
        let request = Self {
            items,
            verdict: Rc::new(RefCell::new(Some(tx))),
        };
        (request, rx)
    }

    pub fn approve(&self) {
        self.answer(true);
    }

    pub fn deny(&self) {
        self.answer(false);
    }

    fn answer(&self, approved: bool) {
        if let Some(tx) = self.verdict.borrow_mut().take() {
            let _ = tx.send(approved);
        }
    }
}

/// One object a request would remove. Names are the document's own text and
/// are only ever displayed, never interpreted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removal {
    Composition { name: String },
    Layer { name: String, comp: String },
    Node { type_key: String, id: String },
}

impl Removal {
    /// The line the dialog shows, in the active locale.
    pub fn text(&self) -> String {
        match self {
            Self::Composition { name } => {
                t!("live_edit.confirm.composition").replace("{name}", name)
            }
            Self::Layer { name, comp } => t!("live_edit.confirm.layer")
                .replace("{name}", name)
                .replace("{comp}", comp),
            Self::Node { type_key, id } => t!("live_edit.confirm.node")
                .replace("{type}", type_key)
                .replace("{id}", id),
        }
    }
}

/// What each removing operation of `ops` would remove, resolved against
/// `doc`. An object the document does not hold yet (an alias an earlier
/// operation of the batch defines) or no longer holds is shown by its alias
/// or id: the user is still told that a removal is coming.
pub fn describe_removals(doc: &Document, ops: &[EditOp]) -> Vec<Removal> {
    let comp_name = |comp: &CompRef| match comp {
        CompRef::Id(id) => doc
            .get_composition(*id)
            .map_or_else(|| format!("#{}", id.raw()), |c| c.name.clone()),
        CompRef::Alias(alias) => alias.clone(),
    };
    let mut lines = Vec::new();
    for op in ops {
        match op {
            EditOp::RemoveComposition { comp } => {
                lines.push(Removal::Composition {
                    name: comp_name(comp),
                });
            }
            EditOp::RemoveLayer { comp, layers } => {
                for layer in layers {
                    let name = match (comp, layer) {
                        (CompRef::Id(c), LayerRef::Id(l)) => doc
                            .get_composition(*c)
                            .and_then(|c| c.get_layer(*l))
                            .map_or_else(|| format!("#{}", l.raw()), |l| l.name.clone()),
                        (_, LayerRef::Id(l)) => format!("#{}", l.raw()),
                        (_, LayerRef::Alias(alias)) => alias.clone(),
                    };
                    lines.push(Removal::Layer {
                        name,
                        comp: comp_name(comp),
                    });
                }
            }
            EditOp::RemoveNode { path, node } => {
                let (type_key, id) = match node {
                    NodeRef::Alias(alias) => (alias.clone(), String::new()),
                    NodeRef::Id(id) => (
                        network_of(doc, path)
                            .and_then(|g| g.node(*id).map(|n| n.type_key.clone()))
                            .unwrap_or_else(|| "?".into()),
                        id.raw().to_string(),
                    ),
                };
                lines.push(Removal::Node { type_key, id });
            }
            _ => {}
        }
    }
    lines
}

fn network_of<'a>(doc: &'a Document, path: &NetworkRef) -> Option<&'a ravel_core::graph::Graph> {
    let (CompRef::Id(comp), LayerRef::Id(layer)) = (&path.comp, &path.layer) else {
        return None;
    };
    let subnets = path
        .subnets
        .iter()
        .map(|n| match n {
            NodeRef::Id(id) => Some(*id),
            NodeRef::Alias(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let path = NetworkPath {
        comp: *comp,
        layer: *layer,
        subnets,
    };
    ravel_core::edit::document::resolve_network(doc, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::edit::CompositionSettings;

    #[test]
    fn a_dropped_request_is_a_denial() {
        let (request, answer) = RemovalRequest::new(vec![]);
        drop(request);
        assert_eq!(futures::executor::block_on(answer).ok(), None);
    }

    #[test]
    fn the_first_answer_stands() {
        let (request, answer) = RemovalRequest::new(vec![]);
        request.deny();
        request.approve();
        assert_eq!(futures::executor::block_on(answer), Ok(false));
    }

    #[test]
    fn a_removal_is_listed_by_the_name_it_has_in_the_document() {
        let doc = Document::default();
        let (doc, comp) = ravel_core::edit::document::add_composition(
            &doc,
            CompositionSettings::fallback("Shot <b>1</b>"),
        );
        let ops = [
            EditOp::RemoveComposition {
                comp: CompRef::Id(comp),
            },
            EditOp::RemoveComposition {
                comp: CompRef::Alias("later".into()),
            },
        ];
        let lines = describe_removals(&doc, &ops);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines,
            [
                Removal::Composition {
                    name: "Shot <b>1</b>".into()
                },
                Removal::Composition {
                    name: "later".into()
                }
            ]
        );
        assert!(describe_removals(&doc, &[]).is_empty());
    }
}

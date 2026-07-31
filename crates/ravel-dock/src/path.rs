// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Addressing nodes inside a [`LayoutNode`] tree.
//!
//! Splits have no identity of their own in the model, so ravel-dock addresses
//! every node by the path of split children taken from the root. Events carry
//! these paths and the helpers here apply them back to a tree.

use ravel_ui::layout::{LayoutNode, PanelInstanceId};

/// Which child of a split a path descends into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitSide {
    /// The leading child (left or top).
    First,
    /// The trailing child (right or bottom).
    Second,
}

/// The route from the tree root to a node. The empty path addresses the root
/// itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NodePath(Vec<SplitSide>);

impl NodePath {
    /// The path to the root node.
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// The path to the `side` child of the node this path addresses.
    pub fn child(&self, side: SplitSide) -> Self {
        let mut steps = self.0.clone();
        steps.push(side);
        Self(steps)
    }

    /// A stable, compact string form (e.g. `"f-s"`), suitable for element ids.
    pub fn id_string(&self) -> String {
        if self.0.is_empty() {
            return "root".to_string();
        }
        self.0
            .iter()
            .map(|s| match s {
                SplitSide::First => "f",
                SplitSide::Second => "s",
            })
            .collect::<Vec<_>>()
            .join("-")
    }
}

/// Looks up the node at `path`, or `None` when the path walks past an area.
pub fn node_at<'a>(node: &'a LayoutNode, path: &NodePath) -> Option<&'a LayoutNode> {
    let mut current = node;
    for side in &path.0 {
        match current {
            LayoutNode::Split { first, second, .. } => {
                current = match side {
                    SplitSide::First => first,
                    SplitSide::Second => second,
                };
            }
            LayoutNode::Area { .. } => return None,
        }
    }
    Some(current)
}

fn node_at_mut<'a>(node: &'a mut LayoutNode, path: &NodePath) -> Option<&'a mut LayoutNode> {
    let mut current = node;
    for side in &path.0 {
        match current {
            LayoutNode::Split { first, second, .. } => {
                current = match side {
                    SplitSide::First => first,
                    SplitSide::Second => second,
                };
            }
            LayoutNode::Area { .. } => return None,
        }
    }
    Some(current)
}

/// Writes `ratio` into the split at `path`. Returns `false` when the path
/// does not address a split or `ratio` is not finite and within `(0.0, 1.0)`
/// (the model's validity invariant).
pub fn set_ratio_at(node: &mut LayoutNode, path: &NodePath, ratio: f32) -> bool {
    if !ratio.is_finite() || ratio <= 0.0 || ratio >= 1.0 {
        return false;
    }
    match node_at_mut(node, path) {
        Some(LayoutNode::Split { ratio: slot, .. }) => {
            *slot = ratio;
            true
        }
        _ => false,
    }
}

/// Activates the tab `instance` inside whichever area hosts it. Returns
/// `false` when the instance is not in the tree.
pub fn activate_tab(node: &mut LayoutNode, instance: PanelInstanceId) -> bool {
    match node {
        LayoutNode::Area { tabs, active } => {
            let Some(pos) = tabs.iter().position(|t| t.id == instance) else {
                return false;
            };
            *active = pos;
            true
        }
        LayoutNode::Split { first, second, .. } => {
            activate_tab(first, instance) || activate_tab(second, instance)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_ui::layout::{Orientation, PanelInstance, PanelInstanceId};
    use ravel_ui::panel::PanelKind;

    fn inst(id: u64) -> PanelInstance {
        PanelInstance::new(PanelInstanceId(id), PanelKind::Viewer)
    }

    /// [Area(0) | (Area(1, 2*) / Area(3))]
    fn tree() -> LayoutNode {
        let mut middle = LayoutNode::area(vec![inst(1), inst(2)]);
        if let LayoutNode::Area { active, .. } = &mut middle {
            *active = 1;
        }
        LayoutNode::split(
            Orientation::Horizontal,
            0.6,
            LayoutNode::area(vec![inst(0)]),
            LayoutNode::split(
                Orientation::Vertical,
                0.7,
                middle,
                LayoutNode::area(vec![inst(3)]),
            ),
        )
    }

    fn path(sides: &[SplitSide]) -> NodePath {
        let mut p = NodePath::root();
        for side in sides {
            p = p.child(*side);
        }
        p
    }

    #[test]
    fn node_at_navigates_splits_and_stops_at_areas() {
        let tree = tree();
        assert_eq!(node_at(&tree, &NodePath::root()), Some(&tree));
        assert_eq!(
            node_at(&tree, &path(&[SplitSide::First])),
            Some(&LayoutNode::area(vec![inst(0)]))
        );
        assert!(matches!(
            node_at(&tree, &path(&[SplitSide::Second])),
            Some(LayoutNode::Split { .. })
        ));
        // Walking past an area yields nothing.
        assert_eq!(
            node_at(&tree, &path(&[SplitSide::First, SplitSide::First])),
            None
        );
    }

    #[test]
    fn node_path_id_strings_are_unique_and_stable() {
        assert_eq!(NodePath::root().id_string(), "root");
        assert_eq!(path(&[SplitSide::First]).id_string(), "f");
        assert_eq!(
            path(&[SplitSide::Second, SplitSide::First]).id_string(),
            "s-f"
        );
    }

    #[test]
    fn set_ratio_at_writes_the_addressed_split() {
        let mut tree = tree();
        assert!(set_ratio_at(&mut tree, &path(&[SplitSide::Second]), 0.25));
        let Some(LayoutNode::Split { second, .. }) = node_at(&tree, &NodePath::root()) else {
            panic!("root must stay a split");
        };
        let Some(LayoutNode::Split { ratio, .. }) = Some(second.as_ref()) else {
            panic!("second must stay a split");
        };
        assert_eq!(*ratio, 0.25);
        assert!(tree.is_valid());
    }

    #[test]
    fn set_ratio_at_rejects_bad_paths_and_ratios() {
        let mut tree = tree();
        let original = tree.clone();
        // Path addresses an area, not a split.
        assert!(!set_ratio_at(&mut tree, &path(&[SplitSide::First]), 0.5));
        // Path does not exist.
        assert!(!set_ratio_at(
            &mut tree,
            &path(&[SplitSide::Second, SplitSide::Second, SplitSide::First]),
            0.5
        ));
        // Out-of-range ratios violate the model invariant.
        assert!(!set_ratio_at(&mut tree, &NodePath::root(), 0.0));
        assert!(!set_ratio_at(&mut tree, &NodePath::root(), 1.0));
        assert!(!set_ratio_at(&mut tree, &NodePath::root(), f32::NAN));
        assert_eq!(tree, original, "rejected writes must not mutate");
    }

    #[test]
    fn activate_tab_switches_the_hosting_area() {
        let mut tree = tree();
        assert!(activate_tab(&mut tree, PanelInstanceId(1)));
        let Some(LayoutNode::Area { tabs, active }) =
            node_at(&tree, &path(&[SplitSide::Second, SplitSide::First]))
        else {
            panic!("middle node must stay an area");
        };
        assert_eq!(tabs[*active].id, PanelInstanceId(1));
        assert!(tree.is_valid());
    }

    #[test]
    fn activate_tab_rejects_unknown_instances() {
        let mut tree = tree();
        let original = tree.clone();
        assert!(!activate_tab(&mut tree, PanelInstanceId(99)));
        assert_eq!(tree, original);
    }
}

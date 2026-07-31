// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Addressing nodes inside a [`LayoutNode`] tree, and applying
//! [`crate::DockEvent`]s back to the model.
//!
//! Splits have no identity of their own in the model, so ravel-dock addresses
//! every node by the path of split children taken from the root. Events carry
//! these paths and the helpers here apply them back to a tree.
//!
//! The tree-level helpers ([`node_at`], [`set_ratio_at`], [`activate_tab`],
//! [`lead_split_child`]) take a single window's [`LayoutNode`]; the
//! event-level appliers ([`apply_tab_drop`], [`apply_area_action`]) take the
//! whole [`WorkspaceLayout`] because a drop or an area action can move a tab
//! between windows and can close a window. Every applier is all-or-nothing:
//! a rejected operation leaves the layout exactly as it was.

use ravel_ui::layout::{
    LayoutError, LayoutNode, Orientation, PanelInstance, PanelInstanceId, WorkspaceLayout,
};
use ravel_ui::window::WindowId;

use crate::dock::AreaAction;
use crate::layout_math::{DEFAULT_SPLIT_RATIO, DropZone};

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

/// Makes the area hosting `instance` the leading (left or top) child of its
/// parent split, swapping it with its sibling and inverting the split ratio so
/// both panes keep their sizes.
///
/// [`WorkspaceLayout::split`] always places the new area *after* the one it
/// came from, which is what a right or bottom drop wants. A left or top drop
/// is the same operation followed by this reordering, so ravel-dock needs no
/// extra model operation (and no change to the persisted representation) to
/// support all four edges.
///
/// Returns `false` when the tree does not host `instance`, or when its area is
/// the whole tree and therefore has no parent split to reorder.
pub fn lead_split_child(node: &mut LayoutNode, instance: PanelInstanceId) -> bool {
    match node {
        LayoutNode::Area { .. } => false,
        LayoutNode::Split {
            ratio,
            first,
            second,
            ..
        } => {
            if is_area_hosting(first, instance) {
                return true;
            }
            if is_area_hosting(second, instance) {
                std::mem::swap(first, second);
                *ratio = 1.0 - *ratio;
                return true;
            }
            lead_split_child(first, instance) || lead_split_child(second, instance)
        }
    }
}

/// `true` when `node` is itself the area holding `instance`.
fn is_area_hosting(node: &LayoutNode, instance: PanelInstanceId) -> bool {
    matches!(node, LayoutNode::Area { tabs, .. } if tabs.iter().any(|t| t.id == instance))
}

/// The tab strip of the area hosting `id`, if this tree hosts it.
fn area_tabs(node: &LayoutNode, id: PanelInstanceId) -> Option<&Vec<PanelInstance>> {
    match node {
        LayoutNode::Area { tabs, .. } => tabs.iter().any(|t| t.id == id).then_some(tabs),
        LayoutNode::Split { first, second, .. } => {
            area_tabs(first, id).or_else(|| area_tabs(second, id))
        }
    }
}

/// Whether dropping `instance` into `zone` of the area hosting `anchor` would
/// change `root`.
///
/// This is the same-window predicate the dock uses to decide whether to
/// highlight a drop zone: a center drop onto the tab's own area changes
/// nothing, and an edge drop out of a single-tab area would leave that area
/// empty (the model rejects it as
/// [`LayoutError::SingleTabArea`]). A tab dragged in from another window is
/// never hosted by `root`, so such drops are resolved by the host instead.
pub fn tab_drop_changes_layout(
    root: &LayoutNode,
    instance: PanelInstanceId,
    anchor: PanelInstanceId,
    zone: DropZone,
) -> bool {
    let Some(source) = area_tabs(root, instance) else {
        return false;
    };
    if area_tabs(root, anchor).is_none() {
        return false;
    }
    let joined = source.iter().any(|t| t.id == anchor);
    match zone {
        DropZone::Center => !joined,
        _ => !joined || source.len() >= 2,
    }
}

/// Applies a [`crate::DockEvent::TabDropped`] to `layout`.
///
/// `window` is the window that emitted the event and `anchor` is any tab of
/// the area under the pointer. A [`DropZone::Center`] drop merges `instance`
/// into that area; an edge drop merges it and then carves it back out into a
/// new sibling area on that edge, which is why no new model operation is
/// needed for drops between areas. `instance` may live in a different window;
/// the model folds away the area (and window) it leaves behind.
pub fn apply_tab_drop(
    layout: &mut WorkspaceLayout,
    window: WindowId,
    instance: PanelInstanceId,
    anchor: PanelInstanceId,
    zone: DropZone,
) -> Result<(), LayoutError> {
    let mut trial = layout.clone();
    let joined = {
        let target = trial
            .window(window)
            .ok_or(LayoutError::UnknownWindow(window))?;
        area_tabs(&target.root, instance).is_some_and(|tabs| tabs.iter().any(|t| t.id == anchor))
    };
    match zone.orientation() {
        None => trial.move_tab(instance, window, anchor)?,
        Some(orientation) => {
            // Merging first gives the target area a second tab, so splitting
            // the dropped tab back out cannot hit `SingleTabArea`.
            if !joined {
                trial.move_tab(instance, window, anchor)?;
            }
            trial.split(window, instance, orientation, DEFAULT_SPLIT_RATIO)?;
            if zone.leads() {
                let root = &mut trial
                    .window_mut(window)
                    .ok_or(LayoutError::UnknownWindow(window))?
                    .root;
                if !lead_split_child(root, instance) {
                    return Err(LayoutError::UnknownInstance(instance));
                }
            }
        }
    }
    debug_assert!(trial.is_valid(), "drop produced an invalid layout");
    *layout = trial;
    Ok(())
}

/// Applies a [`crate::DockEvent::AreaActionRequested`] to `layout`.
///
/// `instance` is the area's active tab, as carried by the event.
pub fn apply_area_action(
    layout: &mut WorkspaceLayout,
    window: WindowId,
    instance: PanelInstanceId,
    action: AreaAction,
) -> Result<(), LayoutError> {
    match action {
        AreaAction::SplitRight => layout.split(
            window,
            instance,
            Orientation::Horizontal,
            DEFAULT_SPLIT_RATIO,
        ),
        AreaAction::SplitDown => {
            layout.split(window, instance, Orientation::Vertical, DEFAULT_SPLIT_RATIO)
        }
        AreaAction::DuplicateRight => {
            let mut trial = layout.clone();
            // The duplicate lands next to `instance`, so the area now holds at
            // least two tabs and the split always succeeds.
            let duplicate = trial.duplicate_instance(instance)?;
            trial.split(
                window,
                duplicate,
                Orientation::Horizontal,
                DEFAULT_SPLIT_RATIO,
            )?;
            debug_assert!(
                trial.is_valid(),
                "duplicate split produced an invalid layout"
            );
            *layout = trial;
            Ok(())
        }
        AreaAction::Close => layout.close_area(window, instance),
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

    /// The fixture tree inside a single-window workspace.
    fn workspace() -> (WorkspaceLayout, WindowId) {
        let layout = WorkspaceLayout::new(tree()).expect("fixture tree is valid");
        let window = layout.main_window().id;
        (layout, window)
    }

    /// A workspace whose main window is one area with a single tab.
    fn lone_area_workspace() -> (WorkspaceLayout, WindowId) {
        let layout =
            WorkspaceLayout::new(LayoutNode::area(vec![inst(0)])).expect("single area is valid");
        let window = layout.main_window().id;
        (layout, window)
    }

    fn main_root(layout: &WorkspaceLayout) -> &LayoutNode {
        &layout.main_window().root
    }

    #[test]
    fn lead_split_child_swaps_the_sibling_and_inverts_the_ratio() {
        let mut tree = LayoutNode::split(
            Orientation::Horizontal,
            0.75,
            LayoutNode::area(vec![inst(0)]),
            LayoutNode::area(vec![inst(1)]),
        );
        assert!(lead_split_child(&mut tree, PanelInstanceId(1)));
        assert_eq!(
            tree,
            LayoutNode::split(
                Orientation::Horizontal,
                0.25,
                LayoutNode::area(vec![inst(1)]),
                LayoutNode::area(vec![inst(0)]),
            )
        );
        assert!(tree.is_valid());
    }

    #[test]
    fn lead_split_child_leaves_an_already_leading_area_alone() {
        let mut tree = tree();
        let original = tree.clone();
        assert!(lead_split_child(&mut tree, PanelInstanceId(0)));
        assert_eq!(tree, original);
        // A nested area reorders only its own parent split, not the root.
        assert!(lead_split_child(&mut tree, PanelInstanceId(3)));
        let Some(LayoutNode::Split {
            orientation,
            ratio,
            first,
            ..
        }) = node_at(&tree, &path(&[SplitSide::Second]))
        else {
            panic!("second child must stay a split");
        };
        assert_eq!(*orientation, Orientation::Vertical);
        assert!((*ratio - 0.3).abs() < 1e-6, "ratio inverted to {ratio}");
        assert_eq!(**first, LayoutNode::area(vec![inst(3)]));
        assert!(tree.is_valid());
    }

    #[test]
    fn lead_split_child_rejects_unknown_instances_and_root_areas() {
        let mut tree = tree();
        let original = tree.clone();
        assert!(!lead_split_child(&mut tree, PanelInstanceId(99)));
        assert_eq!(tree, original);
        let mut lone = LayoutNode::area(vec![inst(0)]);
        assert!(!lead_split_child(&mut lone, PanelInstanceId(0)));
    }

    #[test]
    fn tab_drop_changes_layout_rejects_no_op_drops() {
        let tree = tree();
        // Center onto the tab's own area changes nothing.
        assert!(!tab_drop_changes_layout(
            &tree,
            PanelInstanceId(1),
            PanelInstanceId(2),
            DropZone::Center
        ));
        // Any edge out of a two-tab area is a real split.
        assert!(tab_drop_changes_layout(
            &tree,
            PanelInstanceId(1),
            PanelInstanceId(2),
            DropZone::Bottom
        ));
        // An edge out of a single-tab area would empty it.
        assert!(!tab_drop_changes_layout(
            &tree,
            PanelInstanceId(0),
            PanelInstanceId(0),
            DropZone::Right
        ));
        // Drops onto another area always change something.
        for zone in [
            DropZone::Center,
            DropZone::Left,
            DropZone::Right,
            DropZone::Top,
            DropZone::Bottom,
        ] {
            assert!(tab_drop_changes_layout(
                &tree,
                PanelInstanceId(0),
                PanelInstanceId(3),
                zone
            ));
        }
        // Unknown ids are never droppable.
        assert!(!tab_drop_changes_layout(
            &tree,
            PanelInstanceId(99),
            PanelInstanceId(3),
            DropZone::Center
        ));
        assert!(!tab_drop_changes_layout(
            &tree,
            PanelInstanceId(0),
            PanelInstanceId(99),
            DropZone::Center
        ));
    }

    #[test]
    fn apply_tab_drop_center_merges_into_the_target_area() {
        let (mut layout, window) = workspace();
        apply_tab_drop(
            &mut layout,
            window,
            PanelInstanceId(0),
            PanelInstanceId(3),
            DropZone::Center,
        )
        .expect("center drop merges");
        // The emptied source area folded away, leaving the vertical split.
        assert_eq!(
            main_root(&layout),
            &LayoutNode::split(
                Orientation::Vertical,
                0.7,
                {
                    let mut middle = LayoutNode::area(vec![inst(1), inst(2)]);
                    if let LayoutNode::Area { active, .. } = &mut middle {
                        *active = 1;
                    }
                    middle
                },
                LayoutNode::Area {
                    tabs: vec![inst(3), inst(0)],
                    active: 1,
                },
            )
        );
        assert!(layout.is_valid());
    }

    #[test]
    fn apply_tab_drop_trailing_edges_place_the_tab_after_the_target() {
        for (zone, orientation) in [
            (DropZone::Right, Orientation::Horizontal),
            (DropZone::Bottom, Orientation::Vertical),
        ] {
            let (mut layout, window) = workspace();
            apply_tab_drop(
                &mut layout,
                window,
                PanelInstanceId(0),
                PanelInstanceId(3),
                zone,
            )
            .expect("edge drop splits");
            let Some(LayoutNode::Split {
                orientation: got,
                ratio,
                first,
                second,
            }) = node_at(main_root(&layout), &path(&[SplitSide::Second]))
            else {
                panic!("{zone:?} must leave a split where the target area was");
            };
            assert_eq!(*got, orientation);
            assert_eq!(*ratio, DEFAULT_SPLIT_RATIO);
            assert_eq!(**first, LayoutNode::area(vec![inst(3)]));
            assert_eq!(**second, LayoutNode::area(vec![inst(0)]));
            assert!(layout.is_valid());
        }
    }

    #[test]
    fn apply_tab_drop_leading_edges_place_the_tab_before_the_target() {
        for (zone, orientation) in [
            (DropZone::Left, Orientation::Horizontal),
            (DropZone::Top, Orientation::Vertical),
        ] {
            let (mut layout, window) = workspace();
            apply_tab_drop(
                &mut layout,
                window,
                PanelInstanceId(0),
                PanelInstanceId(3),
                zone,
            )
            .expect("edge drop splits");
            let Some(LayoutNode::Split {
                orientation: got,
                first,
                second,
                ..
            }) = node_at(main_root(&layout), &path(&[SplitSide::Second]))
            else {
                panic!("{zone:?} must leave a split where the target area was");
            };
            assert_eq!(*got, orientation);
            assert_eq!(**first, LayoutNode::area(vec![inst(0)]));
            assert_eq!(**second, LayoutNode::area(vec![inst(3)]));
            assert!(layout.is_valid());
        }
    }

    #[test]
    fn apply_tab_drop_splits_a_tab_out_of_its_own_area() {
        let (mut layout, window) = workspace();
        apply_tab_drop(
            &mut layout,
            window,
            PanelInstanceId(1),
            PanelInstanceId(2),
            DropZone::Right,
        )
        .expect("a two-tab area can split within itself");
        let Some(LayoutNode::Split { first, second, .. }) = node_at(
            main_root(&layout),
            &path(&[SplitSide::Second, SplitSide::First]),
        ) else {
            panic!("the middle area must become a split");
        };
        assert_eq!(**first, LayoutNode::area(vec![inst(2)]));
        assert_eq!(**second, LayoutNode::area(vec![inst(1)]));
        assert!(layout.is_valid());
    }

    #[test]
    fn apply_tab_drop_rejects_emptying_an_area_without_mutating() {
        let (mut layout, window) = workspace();
        let original = layout.clone();
        assert_eq!(
            apply_tab_drop(
                &mut layout,
                window,
                PanelInstanceId(0),
                PanelInstanceId(0),
                DropZone::Right
            ),
            Err(LayoutError::SingleTabArea(PanelInstanceId(0)))
        );
        assert_eq!(layout, original, "a rejected drop must not mutate");
    }

    #[test]
    fn apply_tab_drop_rejects_unknown_windows_and_instances() {
        let (mut layout, window) = workspace();
        let original = layout.clone();
        assert_eq!(
            apply_tab_drop(
                &mut layout,
                WindowId(99),
                PanelInstanceId(0),
                PanelInstanceId(3),
                DropZone::Center
            ),
            Err(LayoutError::UnknownWindow(WindowId(99)))
        );
        assert_eq!(
            apply_tab_drop(
                &mut layout,
                window,
                PanelInstanceId(99),
                PanelInstanceId(3),
                DropZone::Center
            ),
            Err(LayoutError::UnknownInstance(PanelInstanceId(99)))
        );
        assert_eq!(layout, original);
    }

    #[test]
    fn apply_tab_drop_moves_a_tab_between_windows() {
        let (mut layout, main) = workspace();
        layout
            .detach_to_window(PanelInstanceId(3))
            .expect("detaching a tab out of a two-area window");
        assert_eq!(layout.windows().len(), 2);

        apply_tab_drop(
            &mut layout,
            main,
            PanelInstanceId(3),
            PanelInstanceId(0),
            DropZone::Bottom,
        )
        .expect("a tab from another window drops onto an edge");

        assert_eq!(layout.windows().len(), 1, "the emptied window is closed");
        let Some(LayoutNode::Split {
            orientation,
            first,
            second,
            ..
        }) = node_at(main_root(&layout), &path(&[SplitSide::First]))
        else {
            panic!("the drop target area must become a split");
        };
        assert_eq!(*orientation, Orientation::Vertical);
        assert_eq!(**first, LayoutNode::area(vec![inst(0)]));
        assert_eq!(**second, LayoutNode::area(vec![inst(3)]));
        assert!(layout.is_valid());
    }

    #[test]
    fn apply_area_action_splits_the_active_tab_out() {
        for (action, orientation) in [
            (AreaAction::SplitRight, Orientation::Horizontal),
            (AreaAction::SplitDown, Orientation::Vertical),
        ] {
            let (mut layout, window) = workspace();
            apply_area_action(&mut layout, window, PanelInstanceId(1), action)
                .expect("a two-tab area can split");
            let Some(LayoutNode::Split {
                orientation: got,
                first,
                second,
                ..
            }) = node_at(
                main_root(&layout),
                &path(&[SplitSide::Second, SplitSide::First]),
            )
            else {
                panic!("{action:?} must turn the area into a split");
            };
            assert_eq!(*got, orientation);
            assert_eq!(**first, LayoutNode::area(vec![inst(2)]));
            assert_eq!(**second, LayoutNode::area(vec![inst(1)]));
            assert!(layout.is_valid());
        }
    }

    #[test]
    fn apply_area_action_split_rejects_single_tab_areas() {
        let (mut layout, window) = workspace();
        let original = layout.clone();
        for action in [AreaAction::SplitRight, AreaAction::SplitDown] {
            assert_eq!(
                apply_area_action(&mut layout, window, PanelInstanceId(0), action),
                Err(LayoutError::SingleTabArea(PanelInstanceId(0)))
            );
        }
        assert_eq!(layout, original);
    }

    #[test]
    fn apply_area_action_duplicate_right_splits_a_single_tab_area() {
        let (mut layout, window) = workspace();
        apply_area_action(
            &mut layout,
            window,
            PanelInstanceId(0),
            AreaAction::DuplicateRight,
        )
        .expect("duplicating provides the second tab the split needs");
        let Some(LayoutNode::Split {
            orientation,
            first,
            second,
            ..
        }) = node_at(main_root(&layout), &path(&[SplitSide::First]))
        else {
            panic!("the area must become a split");
        };
        assert_eq!(*orientation, Orientation::Horizontal);
        assert_eq!(**first, LayoutNode::area(vec![inst(0)]));
        // The duplicate is a fresh instance of the same panel kind.
        let LayoutNode::Area { tabs, .. } = second.as_ref() else {
            panic!("the new area holds the duplicate");
        };
        assert_eq!(tabs.len(), 1);
        assert_ne!(tabs[0].id, PanelInstanceId(0));
        assert_eq!(tabs[0].kind, PanelKind::Viewer);
        assert!(layout.is_valid());
    }

    #[test]
    fn apply_area_action_close_drops_every_tab_of_the_area() {
        let (mut layout, window) = workspace();
        apply_area_action(&mut layout, window, PanelInstanceId(1), AreaAction::Close)
            .expect("a non-root area closes");
        assert_eq!(
            main_root(&layout),
            &LayoutNode::split(
                Orientation::Horizontal,
                0.6,
                LayoutNode::area(vec![inst(0)]),
                LayoutNode::area(vec![inst(3)]),
            ),
            "both tabs of the closed area are gone"
        );
        assert!(layout.is_valid());
    }

    #[test]
    fn apply_area_action_close_rejects_the_main_window_last_area() {
        let (mut layout, window) = lone_area_workspace();
        let original = layout.clone();
        assert_eq!(
            apply_area_action(&mut layout, window, PanelInstanceId(0), AreaAction::Close),
            Err(LayoutError::MainWindowLastArea)
        );
        assert_eq!(layout, original);
    }
}

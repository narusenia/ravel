// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layout model v2: split/area trees across multiple windows.
//!
//! A workspace is a set of windows ([`WorkspaceLayout`]); each window hosts one
//! layout tree ([`LayoutNode`]). A tree is either a [`LayoutNode::Split`] that
//! divides its area between two subtrees, or a [`LayoutNode::Area`] holding a
//! tab strip of panel instances. The same panel kind may appear any number of
//! times across the whole workspace; each occurrence is a distinct
//! [`PanelInstance`] identified by a [`PanelInstanceId`].
//!
//! All operations are pure state transitions on `&mut self` — no GPUI, no
//! async, no I/O — so the host (`ravel-app`) can drive real windows from them
//! and every rule is unit-testable headlessly.

use crate::panel::{DockSlot, PanelKind};
use crate::window::{WindowId, WindowPlacement};
use serde::{Deserialize, Serialize};

/// Split orientation of a layout node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    /// Children are placed side by side (left / right).
    Horizontal,
    /// Children are stacked (top / bottom).
    Vertical,
}

/// Opaque identifier for a panel instance, unique within a
/// [`WorkspaceLayout`]. Multiple instances may share a [`PanelKind`]; the id
/// is what distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PanelInstanceId(pub u64);

/// One occurrence of a panel in a layout tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelInstance {
    /// Unique identity of this instance.
    pub id: PanelInstanceId,
    /// Which panel this instance shows.
    pub kind: PanelKind,
}

impl PanelInstance {
    /// Creates an instance with the given id and kind.
    pub fn new(id: PanelInstanceId, kind: PanelKind) -> Self {
        Self { id, kind }
    }
}

/// A node in a window's layout tree.
///
/// Areas host a non-empty tab strip of panel instances (`active` indexes into
/// `tabs`); splits divide the available area between two child subtrees by
/// `ratio` (the fraction given to the first child).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    /// A split between two child subtrees.
    Split {
        /// Whether the split is horizontal or vertical.
        orientation: Orientation,
        /// Fraction `(0.0, 1.0)` of the area given to `first`.
        ratio: f32,
        /// The leading child (left or top).
        first: Box<LayoutNode>,
        /// The trailing child (right or bottom).
        second: Box<LayoutNode>,
    },
    /// A tabbed area hosting one or more panel instances.
    Area {
        /// The tabs in this area, left to right. Never empty in a valid tree.
        tabs: Vec<PanelInstance>,
        /// Index of the active tab within `tabs`.
        active: usize,
    },
}

/// Result of removing a single instance from a subtree.
enum RemoveOutcome {
    NotFound,
    Removed {
        instance: PanelInstance,
        /// `true` when the area that held the instance is now empty and must
        /// be folded away by the parent split.
        area_empty: bool,
    },
}

/// Result of splitting the area that contains a given instance.
enum SplitOutcome {
    NotFound,
    /// The area holds only that one tab; splitting would leave it empty.
    SingleTab,
    Done,
}

/// Result of closing the area that contains a given instance.
enum AreaClose {
    NotFound,
    /// This node IS the closed area; the parent split must fold.
    ClosedHere,
    /// The area was closed (and the fold handled) somewhere below.
    ClosedBelow,
}

impl LayoutNode {
    /// Convenience constructor for a split.
    pub fn split(
        orientation: Orientation,
        ratio: f32,
        first: LayoutNode,
        second: LayoutNode,
    ) -> Self {
        LayoutNode::Split {
            orientation,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Convenience constructor for an area; the first tab starts active.
    pub fn area(tabs: Vec<PanelInstance>) -> Self {
        LayoutNode::Area { tabs, active: 0 }
    }

    /// Transient placeholder used while restructuring the tree in place. It is
    /// always overwritten before the tree is observable again.
    fn vacant() -> Self {
        LayoutNode::Area {
            tabs: Vec::new(),
            active: 0,
        }
    }

    /// Collects every panel kind hosted in this subtree, in left-to-right,
    /// top-to-bottom traversal order.
    pub fn panels(&self) -> Vec<PanelKind> {
        self.instances().into_iter().map(|t| t.kind).collect()
    }

    /// Collects every panel instance hosted in this subtree, in left-to-right,
    /// top-to-bottom traversal order.
    pub fn instances(&self) -> Vec<PanelInstance> {
        let mut out = Vec::new();
        self.collect_instances(&mut out);
        out
    }

    fn collect_instances(&self, out: &mut Vec<PanelInstance>) {
        match self {
            LayoutNode::Area { tabs, .. } => out.extend_from_slice(tabs),
            LayoutNode::Split { first, second, .. } => {
                first.collect_instances(out);
                second.collect_instances(out);
            }
        }
    }

    /// Number of areas (tab strips) in this subtree.
    pub fn area_count(&self) -> usize {
        match self {
            LayoutNode::Area { .. } => 1,
            LayoutNode::Split { first, second, .. } => first.area_count() + second.area_count(),
        }
    }

    /// Looks up an instance by id.
    pub fn instance(&self, id: PanelInstanceId) -> Option<PanelInstance> {
        match self {
            LayoutNode::Area { tabs, .. } => tabs.iter().copied().find(|t| t.id == id),
            LayoutNode::Split { first, second, .. } => {
                first.instance(id).or_else(|| second.instance(id))
            }
        }
    }

    /// Returns `true` if this subtree hosts the given instance.
    pub fn contains(&self, id: PanelInstanceId) -> bool {
        self.instance(id).is_some()
    }

    /// Returns `true` if this node is an area with no tabs. Only ever true
    /// transiently, between a removal and the corresponding fold.
    fn is_empty(&self) -> bool {
        matches!(self, LayoutNode::Area { tabs, .. } if tabs.is_empty())
    }

    /// Returns `true` if every split ratio is strictly within `(0.0, 1.0)` and
    /// finite, every area holds at least one tab with `active` in range, and
    /// instance ids are unique within the tree.
    pub fn is_valid(&self) -> bool {
        let ids = self.instances();
        let unique = ids
            .iter()
            .map(|t| t.id)
            .collect::<std::collections::HashSet<_>>();
        unique.len() == ids.len() && self.is_structurally_valid()
    }

    fn is_structurally_valid(&self) -> bool {
        match self {
            LayoutNode::Area { tabs, active } => !tabs.is_empty() && *active < tabs.len(),
            LayoutNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                ratio.is_finite()
                    && *ratio > 0.0
                    && *ratio < 1.0
                    && first.is_structurally_valid()
                    && second.is_structurally_valid()
            }
        }
    }

    /// Appends `tab` to the area containing `anchor` and activates it.
    /// Returns `false` if no area contains `anchor`.
    fn insert_tab(&mut self, anchor: PanelInstanceId, tab: PanelInstance) -> bool {
        match self {
            LayoutNode::Area { tabs, active } => {
                if tabs.iter().any(|t| t.id == anchor) {
                    tabs.push(tab);
                    *active = tabs.len() - 1;
                    true
                } else {
                    false
                }
            }
            LayoutNode::Split { first, second, .. } => {
                first.insert_tab(anchor, tab) || second.insert_tab(anchor, tab)
            }
        }
    }

    /// Removes `id` from its area, folding away splits whose child area became
    /// empty. The root node itself is never folded away — it becomes an empty
    /// area instead, which callers must handle.
    fn remove_instance(&mut self, id: PanelInstanceId) -> RemoveOutcome {
        match self {
            LayoutNode::Area { tabs, active } => {
                let Some(pos) = tabs.iter().position(|t| t.id == id) else {
                    return RemoveOutcome::NotFound;
                };
                let instance = tabs.remove(pos);
                if tabs.is_empty() {
                    *active = 0;
                    return RemoveOutcome::Removed {
                        instance,
                        area_empty: true,
                    };
                }
                if *active > pos {
                    *active -= 1;
                }
                if *active >= tabs.len() {
                    *active = tabs.len() - 1;
                }
                RemoveOutcome::Removed {
                    instance,
                    area_empty: false,
                }
            }
            LayoutNode::Split { first, second, .. } => {
                match first.remove_instance(id) {
                    RemoveOutcome::Removed {
                        instance,
                        area_empty: true,
                    } => {
                        // Fold the split: the second subtree takes our place.
                        *self = std::mem::replace(second.as_mut(), LayoutNode::vacant());
                        RemoveOutcome::Removed {
                            instance,
                            area_empty: false,
                        }
                    }
                    RemoveOutcome::NotFound => match second.remove_instance(id) {
                        RemoveOutcome::Removed {
                            instance,
                            area_empty: true,
                        } => {
                            *self = std::mem::replace(first.as_mut(), LayoutNode::vacant());
                            RemoveOutcome::Removed {
                                instance,
                                area_empty: false,
                            }
                        }
                        other => other,
                    },
                    other => other,
                }
            }
        }
    }

    /// Moves the tab `id` out of its area into a new sibling area placed after
    /// it (`second`) along `orientation`.
    fn split_area(
        &mut self,
        id: PanelInstanceId,
        orientation: Orientation,
        ratio: f32,
    ) -> SplitOutcome {
        match self {
            LayoutNode::Area { tabs, active } => {
                let Some(pos) = tabs.iter().position(|t| t.id == id) else {
                    return SplitOutcome::NotFound;
                };
                if tabs.len() < 2 {
                    return SplitOutcome::SingleTab;
                }
                let instance = tabs.remove(pos);
                if *active > pos {
                    *active -= 1;
                }
                if *active >= tabs.len() {
                    *active = tabs.len() - 1;
                }
                let old = std::mem::replace(self, LayoutNode::vacant());
                *self =
                    LayoutNode::split(orientation, ratio, old, LayoutNode::area(vec![instance]));
                SplitOutcome::Done
            }
            LayoutNode::Split { first, second, .. } => {
                match first.split_area(id, orientation, ratio) {
                    SplitOutcome::NotFound => second.split_area(id, orientation, ratio),
                    other => other,
                }
            }
        }
    }

    /// Destroys the area containing `id` (dropping all of its tabs) and folds
    /// the parent split. Only the direct parent split folds; the fold is not
    /// propagated further up. The caller handles the case where the root
    /// itself is the target area.
    fn close_area(&mut self, id: PanelInstanceId) -> AreaClose {
        match self {
            LayoutNode::Area { tabs, .. } => {
                if tabs.iter().any(|t| t.id == id) {
                    AreaClose::ClosedHere
                } else {
                    AreaClose::NotFound
                }
            }
            LayoutNode::Split { first, second, .. } => match first.close_area(id) {
                AreaClose::ClosedHere => {
                    *self = std::mem::replace(second.as_mut(), LayoutNode::vacant());
                    AreaClose::ClosedBelow
                }
                AreaClose::ClosedBelow => AreaClose::ClosedBelow,
                AreaClose::NotFound => match second.close_area(id) {
                    AreaClose::ClosedHere => {
                        *self = std::mem::replace(first.as_mut(), LayoutNode::vacant());
                        AreaClose::ClosedBelow
                    }
                    other => other,
                },
            },
        }
    }

    /// Inserts `new_instance` directly after the tab `id` in its area and
    /// activates it. Returns `false` if `id` is not hosted here.
    fn duplicate(&mut self, id: PanelInstanceId, new_instance: PanelInstance) -> bool {
        match self {
            LayoutNode::Area { tabs, active } => {
                let Some(pos) = tabs.iter().position(|t| t.id == id) else {
                    return false;
                };
                tabs.insert(pos + 1, new_instance);
                *active = pos + 1;
                true
            }
            LayoutNode::Split { first, second, .. } => {
                first.duplicate(id, new_instance) || second.duplicate(id, new_instance)
            }
        }
    }

    /// Makes the tab `id` the active tab of its area. Returns `false` if `id`
    /// is not hosted here.
    fn activate(&mut self, id: PanelInstanceId) -> bool {
        match self {
            LayoutNode::Area { tabs, active } => {
                let Some(pos) = tabs.iter().position(|t| t.id == id) else {
                    return false;
                };
                *active = pos;
                true
            }
            LayoutNode::Split { first, second, .. } => first.activate(id) || second.activate(id),
        }
    }

    /// The id of the first tab of the area that adopts new panels for `slot`.
    ///
    /// The descent walks toward the slot's edge: left takes the `first` child
    /// of horizontal splits, right and bottom take the `second` child of
    /// horizontal and vertical splits respectively. At a split whose
    /// orientation does not lead toward the slot (and always for
    /// [`DockSlot::Center`]) the larger child is followed, so the primary
    /// region of the window adopts center panels. A valid tree always
    /// bottoms out in a non-empty area.
    fn slot_anchor(&self, slot: DockSlot) -> PanelInstanceId {
        match self {
            LayoutNode::Area { tabs, .. } => {
                debug_assert!(!tabs.is_empty(), "valid trees have no empty areas");
                tabs[0].id
            }
            LayoutNode::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                let go_first = match (slot, orientation) {
                    (DockSlot::Left, Orientation::Horizontal) => true,
                    (DockSlot::Right, Orientation::Horizontal) => false,
                    (DockSlot::Bottom, Orientation::Vertical) => false,
                    _ => *ratio >= 0.5,
                };
                if go_first {
                    first.slot_anchor(slot)
                } else {
                    second.slot_anchor(slot)
                }
            }
        }
    }

    /// Reassigns instance ids in left-to-right, top-to-bottom traversal order,
    /// drawing from `next`. Used when adopting an externally built tree (a
    /// preset) whose deterministic ids may collide with live instances.
    fn renumber(&mut self, next: &mut u64) {
        match self {
            LayoutNode::Area { tabs, .. } => {
                for tab in tabs {
                    tab.id = PanelInstanceId(*next);
                    *next += 1;
                }
            }
            LayoutNode::Split { first, second, .. } => {
                first.renumber(next);
                second.renumber(next);
            }
        }
    }
}

/// One window of the workspace: a layout tree plus host-side placement data.
///
/// The window at index 0 of [`WorkspaceLayout::windows`] is the main window by
/// convention; all other windows are detached windows created by
/// [`WorkspaceLayout::detach_to_window`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowLayout {
    /// Logical window identifier. The host keeps the mapping to real OS
    /// window handles.
    pub id: WindowId,
    /// The root of this window's layout tree.
    pub root: LayoutNode,
    /// Last known on-desktop placement, if recorded (used to restore
    /// multi-monitor arrangements).
    pub placement: Option<WindowPlacement>,
    /// Whether the window floats above others (detached windows only).
    pub always_on_top: bool,
}

/// Errors produced by [`WorkspaceLayout`] operations.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LayoutError {
    /// No window has the given id.
    #[error("no window with id {0:?}")]
    UnknownWindow(WindowId),
    /// No panel instance has the given id.
    #[error("no panel instance with id {0:?}")]
    UnknownInstance(PanelInstanceId),
    /// A split ratio was not finite and within `(0.0, 1.0)`.
    #[error("split ratio {0} is out of range (0.0, 1.0)")]
    InvalidRatio(f32),
    /// Splitting a single-tab area would leave it empty; duplicate the tab
    /// first if a second area of the same panel is wanted.
    #[error("cannot split the area of {0:?}: it has only one tab")]
    SingleTabArea(PanelInstanceId),
    /// The operation would leave the main window without any area.
    #[error("the main window must keep at least one area")]
    MainWindowLastArea,
    /// The main window cannot be closed through the layout model (closing it
    /// quits the application and is the host's concern).
    #[error("the main window cannot be closed")]
    MainWindowClose,
}

/// Reasons a [`WorkspaceLayout`] can be structurally invalid. Checked on
/// construction and deserialization so an invalid layout can never exist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutValidationError {
    /// The workspace has no windows at all (`windows[0]` would not exist).
    #[error("workspace layout has no windows")]
    NoWindows,
    /// Two windows share the same id.
    #[error("duplicate window id {0:?}")]
    DuplicateWindowId(WindowId),
    /// Two panel instances (possibly in different windows) share the same id.
    #[error("duplicate panel instance id {0:?}")]
    DuplicateInstanceId(PanelInstanceId),
    /// A window's layout tree is invalid (bad ratio, empty area, out-of-range
    /// active tab, or duplicate ids within the tree).
    #[error("window {0:?} has an invalid layout tree")]
    InvalidTree(WindowId),
    /// `next_window_id` is not ahead of every window id in use; new windows
    /// would reuse an existing id.
    #[error("window id {0:?} is not below next_window_id")]
    StaleWindowIdCounter(WindowId),
    /// `next_instance_id` is not ahead of every instance id in use; new
    /// instances would reuse an existing id.
    #[error("panel instance id {0:?} is not below next_instance_id")]
    StaleInstanceIdCounter(PanelInstanceId),
}

/// The whole workspace: every window and its layout tree.
///
/// `windows[0]` is the main window by convention. The model guarantees the
/// main window always exists and always keeps at least one area; detached
/// windows are removed automatically once their last area disappears.
///
/// Invariants (non-empty `windows`, unique window/instance ids, valid trees,
/// id counters ahead of every id in use) are enforced on construction
/// ([`WorkspaceLayout::new`]) and on deserialization — an invalid document is
/// a parse error, so callers can fall back to a default layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WorkspaceLayoutWire")]
pub struct WorkspaceLayout {
    windows: Vec<WindowLayout>,
    next_window_id: u64,
    next_instance_id: u64,
}

/// Deserialization shadow of [`WorkspaceLayout`]. Validation happens in the
/// `TryFrom` conversion, so a parsed `WorkspaceLayout` is always valid.
#[derive(Deserialize)]
struct WorkspaceLayoutWire {
    windows: Vec<WindowLayout>,
    next_window_id: u64,
    next_instance_id: u64,
}

impl TryFrom<WorkspaceLayoutWire> for WorkspaceLayout {
    type Error = LayoutValidationError;

    fn try_from(wire: WorkspaceLayoutWire) -> Result<Self, Self::Error> {
        let layout = WorkspaceLayout {
            windows: wire.windows,
            next_window_id: wire.next_window_id,
            next_instance_id: wire.next_instance_id,
        };
        layout.validate()?;
        Ok(layout)
    }
}

impl WorkspaceLayout {
    /// Creates a workspace with a single main window hosting `main_root`.
    ///
    /// Fails if `main_root` is not a valid tree (bad ratio, empty area,
    /// out-of-range active tab, or duplicate instance ids).
    pub fn new(main_root: LayoutNode) -> Result<Self, LayoutValidationError> {
        if !main_root.is_valid() {
            return Err(LayoutValidationError::InvalidTree(WindowId(0)));
        }
        let next_instance_id = main_root
            .instances()
            .iter()
            .map(|t| t.id.0)
            .max()
            .map_or(0, |max| max + 1);
        Ok(Self {
            windows: vec![WindowLayout {
                id: WindowId(0),
                root: main_root,
                placement: None,
                always_on_top: false,
            }],
            next_window_id: 1,
            next_instance_id,
        })
    }

    /// All windows, main window first.
    pub fn windows(&self) -> &[WindowLayout] {
        &self.windows
    }

    /// The main window (`windows[0]`).
    pub fn main_window(&self) -> &WindowLayout {
        &self.windows[0]
    }

    /// Mutable access to the main window (e.g. preset switches replacing the
    /// main tree).
    pub fn main_window_mut(&mut self) -> &mut WindowLayout {
        &mut self.windows[0]
    }

    /// Looks up a window by id.
    pub fn window(&self, id: WindowId) -> Option<&WindowLayout> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// Mutable access to a window by id.
    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut WindowLayout> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// Finds which window hosts `id`, and the instance itself.
    pub fn find_instance(&self, id: PanelInstanceId) -> Option<(WindowId, PanelInstance)> {
        self.windows
            .iter()
            .find_map(|w| w.root.instance(id).map(|inst| (w.id, inst)))
    }

    /// Checks every structural invariant: at least one window, unique window
    /// and instance ids across the whole workspace, valid trees, and id
    /// counters ahead of every id in use.
    pub fn validate(&self) -> Result<(), LayoutValidationError> {
        if self.windows.is_empty() {
            return Err(LayoutValidationError::NoWindows);
        }
        let mut window_ids = std::collections::HashSet::new();
        let mut instance_ids = std::collections::HashSet::new();
        for window in &self.windows {
            if !window_ids.insert(window.id) {
                return Err(LayoutValidationError::DuplicateWindowId(window.id));
            }
            if window.id.0 >= self.next_window_id {
                return Err(LayoutValidationError::StaleWindowIdCounter(window.id));
            }
            if !window.root.is_valid() {
                return Err(LayoutValidationError::InvalidTree(window.id));
            }
            for tab in window.root.instances() {
                if !instance_ids.insert(tab.id) {
                    return Err(LayoutValidationError::DuplicateInstanceId(tab.id));
                }
                if tab.id.0 >= self.next_instance_id {
                    return Err(LayoutValidationError::StaleInstanceIdCounter(tab.id));
                }
            }
        }
        Ok(())
    }

    /// Returns `true` if [`WorkspaceLayout::validate`] passes.
    pub fn is_valid(&self) -> bool {
        self.validate().is_ok()
    }

    fn window_index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn window_index_containing(&self, instance: PanelInstanceId) -> Option<usize> {
        self.windows.iter().position(|w| w.root.contains(instance))
    }

    /// `true` if removing `instance` from window `index` would empty the main
    /// window's root area (its last tab in its last area).
    fn leaves_main_empty(&self, index: usize, instance: PanelInstanceId) -> bool {
        index == 0
            && matches!(
                &self.windows[0].root,
                LayoutNode::Area { tabs, .. } if tabs.len() == 1 && tabs[0].id == instance
            )
    }

    /// Moves the tab `instance` out of its area into a new sibling area placed
    /// after it, along `orientation`. The original area keeps the `ratio`
    /// fraction of the space. Fails for single-tab areas (nothing would be
    /// left behind) and for out-of-range ratios.
    pub fn split(
        &mut self,
        window: WindowId,
        instance: PanelInstanceId,
        orientation: Orientation,
        ratio: f32,
    ) -> Result<(), LayoutError> {
        if !ratio.is_finite() || ratio <= 0.0 || ratio >= 1.0 {
            return Err(LayoutError::InvalidRatio(ratio));
        }
        let index = self
            .window_index(window)
            .ok_or(LayoutError::UnknownWindow(window))?;
        match self.windows[index]
            .root
            .split_area(instance, orientation, ratio)
        {
            SplitOutcome::Done => Ok(()),
            SplitOutcome::SingleTab => Err(LayoutError::SingleTabArea(instance)),
            SplitOutcome::NotFound => Err(LayoutError::UnknownInstance(instance)),
        }
    }

    /// Destroys the area containing `instance`, dropping all of its tabs, and
    /// folds the parent split. Closing the last area of a detached window
    /// closes that window; the main window's last area cannot be closed.
    pub fn close_area(
        &mut self,
        window: WindowId,
        instance: PanelInstanceId,
    ) -> Result<(), LayoutError> {
        let index = self
            .window_index(window)
            .ok_or(LayoutError::UnknownWindow(window))?;
        let root_is_target = matches!(
            &self.windows[index].root,
            LayoutNode::Area { tabs, .. } if tabs.iter().any(|t| t.id == instance)
        );
        if root_is_target {
            if index == 0 {
                return Err(LayoutError::MainWindowLastArea);
            }
            self.windows.remove(index);
            return Ok(());
        }
        match self.windows[index].root.close_area(instance) {
            AreaClose::ClosedHere | AreaClose::ClosedBelow => Ok(()),
            AreaClose::NotFound => Err(LayoutError::UnknownInstance(instance)),
        }
    }

    /// Moves the tab `instance` into the area containing `anchor` in window
    /// `target_window`, appending it as the active tab. The source area is
    /// folded away if it becomes empty; a detached window whose last area
    /// disappears is closed. Moving a tab onto itself is a no-op.
    pub fn move_tab(
        &mut self,
        instance: PanelInstanceId,
        target_window: WindowId,
        anchor: PanelInstanceId,
    ) -> Result<(), LayoutError> {
        let src = self
            .window_index_containing(instance)
            .ok_or(LayoutError::UnknownInstance(instance))?;
        let dst = self
            .window_index(target_window)
            .ok_or(LayoutError::UnknownWindow(target_window))?;
        if instance == anchor {
            return if self.windows[dst].root.contains(instance) {
                Ok(())
            } else {
                Err(LayoutError::UnknownInstance(anchor))
            };
        }
        if !self.windows[dst].root.contains(anchor) {
            return Err(LayoutError::UnknownInstance(anchor));
        }
        if self.leaves_main_empty(src, instance) {
            return Err(LayoutError::MainWindowLastArea);
        }
        let RemoveOutcome::Removed { instance: tab, .. } =
            self.windows[src].root.remove_instance(instance)
        else {
            return Err(LayoutError::UnknownInstance(instance));
        };
        self.windows[dst].root.insert_tab(anchor, tab);
        if src != dst && self.windows[src].root.is_empty() {
            self.windows.remove(src);
        }
        Ok(())
    }

    /// Moves the tab `instance` into a brand-new detached window and returns
    /// the new window's id. The source area is folded away if it becomes
    /// empty; a detached source window whose last area disappears is closed.
    pub fn detach_to_window(&mut self, instance: PanelInstanceId) -> Result<WindowId, LayoutError> {
        let src = self
            .window_index_containing(instance)
            .ok_or(LayoutError::UnknownInstance(instance))?;
        if self.leaves_main_empty(src, instance) {
            return Err(LayoutError::MainWindowLastArea);
        }
        let RemoveOutcome::Removed { instance: tab, .. } =
            self.windows[src].root.remove_instance(instance)
        else {
            return Err(LayoutError::UnknownInstance(instance));
        };
        if src != 0 && self.windows[src].root.is_empty() {
            self.windows.remove(src);
        }
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        self.windows.push(WindowLayout {
            id,
            root: LayoutNode::area(vec![tab]),
            placement: None,
            always_on_top: false,
        });
        Ok(id)
    }

    /// Closes a detached window, discarding every instance it hosts. The main
    /// window cannot be closed through the layout model.
    pub fn close_window(&mut self, id: WindowId) -> Result<(), LayoutError> {
        let index = self
            .window_index(id)
            .ok_or(LayoutError::UnknownWindow(id))?;
        if index == 0 {
            return Err(LayoutError::MainWindowClose);
        }
        self.windows.remove(index);
        Ok(())
    }

    /// Creates a second instance of the same panel kind as `instance`,
    /// inserted directly after it in the same area and activated. Returns the
    /// new instance's id.
    pub fn duplicate_instance(
        &mut self,
        instance: PanelInstanceId,
    ) -> Result<PanelInstanceId, LayoutError> {
        let index = self
            .window_index_containing(instance)
            .ok_or(LayoutError::UnknownInstance(instance))?;
        let kind = self.windows[index]
            .root
            .instance(instance)
            .map(|t| t.kind)
            .ok_or(LayoutError::UnknownInstance(instance))?;
        let new_id = PanelInstanceId(self.next_instance_id);
        self.next_instance_id += 1;
        let inserted = self.windows[index]
            .root
            .duplicate(instance, PanelInstance::new(new_id, kind));
        debug_assert!(inserted, "instance was found above");
        Ok(new_id)
    }

    /// Creates a new instance of `kind` and inserts it into `window` at the
    /// panel's [`PanelKind::default_slot`]: the area nearest that slot's edge
    /// adopts the instance as its active tab. Returns the new instance's id,
    /// allocated from the workspace counter.
    pub fn insert_instance(
        &mut self,
        window: WindowId,
        kind: PanelKind,
    ) -> Result<PanelInstanceId, LayoutError> {
        let index = self
            .window_index(window)
            .ok_or(LayoutError::UnknownWindow(window))?;
        let id = PanelInstanceId(self.next_instance_id);
        self.next_instance_id += 1;
        let tab = PanelInstance::new(id, kind);
        let anchor = self.windows[index].root.slot_anchor(kind.default_slot());
        let inserted = self.windows[index].root.insert_tab(anchor, tab);
        debug_assert!(inserted, "anchor was resolved from the same tree");
        Ok(id)
    }

    /// Makes `instance` the active tab of its area. Purely presentational —
    /// no structural change.
    pub fn activate_tab(&mut self, instance: PanelInstanceId) -> Result<(), LayoutError> {
        let index = self
            .window_index_containing(instance)
            .ok_or(LayoutError::UnknownInstance(instance))?;
        if self.windows[index].root.activate(instance) {
            Ok(())
        } else {
            Err(LayoutError::UnknownInstance(instance))
        }
    }

    /// Removes `instance` from its area, folding away splits whose child area
    /// became empty and closing a detached window whose last area disappears.
    /// The main window's last tab cannot be removed. Returns the removed
    /// instance.
    pub fn remove_instance(
        &mut self,
        instance: PanelInstanceId,
    ) -> Result<PanelInstance, LayoutError> {
        let index = self
            .window_index_containing(instance)
            .ok_or(LayoutError::UnknownInstance(instance))?;
        if self.leaves_main_empty(index, instance) {
            return Err(LayoutError::MainWindowLastArea);
        }
        let RemoveOutcome::Removed { instance: tab, .. } =
            self.windows[index].root.remove_instance(instance)
        else {
            return Err(LayoutError::UnknownInstance(instance));
        };
        if index != 0 && self.windows[index].root.is_empty() {
            self.windows.remove(index);
        }
        Ok(tab)
    }

    /// Replaces the main window's tree with `root` (e.g. a preset switch),
    /// reassigning every instance id from the workspace counter so the new
    /// tree can never collide with instances living in detached windows.
    /// Detached windows are left untouched. Fails if `root` is not a valid
    /// tree; on failure the layout is unchanged.
    pub fn replace_main_tree(&mut self, mut root: LayoutNode) -> Result<(), LayoutValidationError> {
        if !root.is_valid() {
            return Err(LayoutValidationError::InvalidTree(self.windows[0].id));
        }
        let mut next = self.next_instance_id;
        root.renumber(&mut next);
        self.next_instance_id = next;
        self.windows[0].root = root;
        debug_assert!(self.is_valid());
        Ok(())
    }

    /// Moves every instance hosted by the detached window `id` into the main
    /// window — each to its [`PanelKind::default_slot`] — and closes the
    /// window. Instance ids are preserved so per-instance view state survives
    /// the move. Returns the moved instances in traversal order. The main
    /// window itself cannot be absorbed.
    pub fn absorb_window(&mut self, id: WindowId) -> Result<Vec<PanelInstance>, LayoutError> {
        let index = self
            .window_index(id)
            .ok_or(LayoutError::UnknownWindow(id))?;
        if index == 0 {
            return Err(LayoutError::MainWindowClose);
        }
        let instances = self.windows[index].root.instances();
        for instance in &instances {
            let RemoveOutcome::Removed { instance: tab, .. } =
                self.windows[index].root.remove_instance(instance.id)
            else {
                return Err(LayoutError::UnknownInstance(instance.id));
            };
            let anchor = self.windows[0].root.slot_anchor(tab.kind.default_slot());
            let inserted = self.windows[0].root.insert_tab(anchor, tab);
            debug_assert!(inserted, "anchor was resolved from the same tree");
        }
        self.windows.remove(index);
        debug_assert!(self.is_valid());
        Ok(instances)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Orientation::{Horizontal, Vertical};
    use PanelKind::*;

    fn inst(id: u64, kind: PanelKind) -> PanelInstance {
        PanelInstance::new(PanelInstanceId(id), kind)
    }

    fn area1(id: u64, kind: PanelKind) -> LayoutNode {
        LayoutNode::area(vec![inst(id, kind)])
    }

    /// [Viewer#0 | (Timeline#1, NodeGraph#2*) / Outliner#3]
    fn sample_tree() -> LayoutNode {
        let mut middle = LayoutNode::area(vec![inst(1, Timeline), inst(2, NodeGraph)]);
        if let LayoutNode::Area { active, .. } = &mut middle {
            *active = 1;
        }
        LayoutNode::split(
            Horizontal,
            0.6,
            area1(0, Viewer),
            LayoutNode::split(Vertical, 0.7, middle, area1(3, Outliner)),
        )
    }

    fn workspace() -> WorkspaceLayout {
        WorkspaceLayout::new(sample_tree()).unwrap()
    }

    // -- structure & validity ------------------------------------------------

    #[test]
    fn instances_traverse_in_order() {
        let tree = sample_tree();
        assert_eq!(
            tree.instances(),
            vec![
                inst(0, Viewer),
                inst(1, Timeline),
                inst(2, NodeGraph),
                inst(3, Outliner)
            ]
        );
        assert_eq!(tree.panels(), vec![Viewer, Timeline, NodeGraph, Outliner]);
        assert_eq!(tree.area_count(), 3);
        assert!(tree.is_valid());
    }

    #[test]
    fn invalid_structures_are_rejected() {
        let bad_ratio = LayoutNode::split(Horizontal, 1.5, area1(0, Viewer), area1(1, Timeline));
        assert!(!bad_ratio.is_valid());

        let empty_area = LayoutNode::area(vec![]);
        assert!(!empty_area.is_valid());

        let out_of_range_active = LayoutNode::Area {
            tabs: vec![inst(0, Viewer)],
            active: 1,
        };
        assert!(!out_of_range_active.is_valid());

        let duplicate_ids =
            LayoutNode::split(Horizontal, 0.5, area1(0, Viewer), area1(0, Timeline));
        assert!(!duplicate_ids.is_valid());
    }

    #[test]
    fn serde_roundtrips_toml_and_json() {
        let tree = sample_tree();
        let toml = toml::to_string_pretty(&tree).unwrap();
        assert_eq!(toml::from_str::<LayoutNode>(&toml).unwrap(), tree);
        let json = serde_json::to_string_pretty(&tree).unwrap();
        assert_eq!(serde_json::from_str::<LayoutNode>(&json).unwrap(), tree);
    }

    #[test]
    fn workspace_serde_roundtrips_toml_and_json() {
        let mut ws = workspace();
        ws.detach_to_window(PanelInstanceId(1)).unwrap();
        ws.duplicate_instance(PanelInstanceId(0)).unwrap();
        let toml = toml::to_string_pretty(&ws).unwrap();
        assert_eq!(toml::from_str::<WorkspaceLayout>(&toml).unwrap(), ws);
        let json = serde_json::to_string_pretty(&ws).unwrap();
        assert_eq!(serde_json::from_str::<WorkspaceLayout>(&json).unwrap(), ws);
    }

    // -- split ---------------------------------------------------------------

    #[test]
    fn split_moves_tab_into_new_sibling_area() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        ws.split(main, PanelInstanceId(2), Horizontal, 0.75)
            .unwrap();
        assert!(ws.is_valid());
        // The middle area keeps Timeline; NodeGraph moves to a new area.
        let root = &ws.main_window().root;
        assert_eq!(root.area_count(), 4);
        assert_eq!(
            root.instances(),
            vec![
                inst(0, Viewer),
                inst(1, Timeline),
                inst(2, NodeGraph),
                inst(3, Outliner)
            ]
        );
        // New area holds only NodeGraph and sits next to the old middle area,
        // inside the original vertical split.
        let LayoutNode::Split { second, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Split { first: middle, .. } = second.as_ref() else {
            panic!("second subtree should still be a split");
        };
        let LayoutNode::Split {
            first,
            ratio,
            orientation,
            ..
        } = middle.as_ref()
        else {
            panic!("middle area should have become a split");
        };
        assert_eq!(*orientation, Horizontal);
        assert_eq!(*ratio, 0.75);
        assert_eq!(first.instances(), vec![inst(1, Timeline)]);
    }

    #[test]
    fn split_rejects_bad_ratio_unknown_ids_and_single_tab_area() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        assert_eq!(
            ws.split(main, PanelInstanceId(2), Horizontal, 1.0),
            Err(LayoutError::InvalidRatio(1.0))
        );
        assert_eq!(
            ws.split(WindowId(99), PanelInstanceId(2), Horizontal, 0.5),
            Err(LayoutError::UnknownWindow(WindowId(99)))
        );
        assert_eq!(
            ws.split(main, PanelInstanceId(42), Horizontal, 0.5),
            Err(LayoutError::UnknownInstance(PanelInstanceId(42)))
        );
        // Viewer sits alone in its area.
        assert_eq!(
            ws.split(main, PanelInstanceId(0), Horizontal, 0.5),
            Err(LayoutError::SingleTabArea(PanelInstanceId(0)))
        );
        assert_eq!(
            ws.main_window().root,
            sample_tree(),
            "failed splits must not mutate"
        );
    }

    // -- close_area ----------------------------------------------------------

    #[test]
    fn close_area_folds_parent_split() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        ws.close_area(main, PanelInstanceId(1)).unwrap();
        assert!(ws.is_valid());
        // Middle area (Timeline + NodeGraph) is gone; its split collapsed to
        // the Outliner area.
        let root = &ws.main_window().root;
        assert_eq!(root.instances(), vec![inst(0, Viewer), inst(3, Outliner)]);
        assert_eq!(root.area_count(), 2);
    }

    #[test]
    fn close_area_on_main_root_is_rejected() {
        let mut ws = WorkspaceLayout::new(area1(0, Viewer)).unwrap();
        let main = ws.main_window().id;
        assert_eq!(
            ws.close_area(main, PanelInstanceId(0)),
            Err(LayoutError::MainWindowLastArea)
        );
        assert_eq!(ws.main_window().root, area1(0, Viewer));
    }

    #[test]
    fn close_area_last_area_of_detached_window_closes_it() {
        let mut ws = workspace();
        let detached = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        assert_eq!(ws.windows().len(), 2);
        ws.close_area(detached, PanelInstanceId(1)).unwrap();
        assert_eq!(ws.windows().len(), 1);
        assert!(ws.window(detached).is_none());
        assert!(ws.is_valid());
    }

    // -- move_tab ------------------------------------------------------------

    #[test]
    fn move_tab_between_areas_appends_and_activates() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        // Move NodeGraph into the Outliner area.
        ws.move_tab(PanelInstanceId(2), main, PanelInstanceId(3))
            .unwrap();
        assert!(ws.is_valid());
        let root = &ws.main_window().root;
        assert_eq!(
            root.instances(),
            vec![
                inst(0, Viewer),
                inst(1, Timeline),
                inst(3, Outliner),
                inst(2, NodeGraph)
            ]
        );
        // Target area has NodeGraph active. The middle area keeps Timeline,
        // so the Outliner area is still nested in the vertical split.
        let LayoutNode::Split { second, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Split {
            second: outliner, ..
        } = second.as_ref()
        else {
            panic!("second subtree should still be a split");
        };
        let LayoutNode::Area { tabs, active } = outliner.as_ref() else {
            panic!("outliner should still be an area");
        };
        assert_eq!(tabs[*active].id, PanelInstanceId(2));
    }

    #[test]
    fn moving_last_tab_out_of_area_folds_split() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        // Move Viewer (alone in its area) into the middle area: its area and
        // the top split must fold away.
        ws.move_tab(PanelInstanceId(0), main, PanelInstanceId(1))
            .unwrap();
        assert!(ws.is_valid());
        let root = &ws.main_window().root;
        assert_eq!(root.area_count(), 2);
        assert_eq!(
            root.instances(),
            vec![
                inst(1, Timeline),
                inst(2, NodeGraph),
                inst(0, Viewer),
                inst(3, Outliner)
            ]
        );
    }

    #[test]
    fn move_tab_across_windows_closes_emptied_detached_window() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        let detached = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        assert_eq!(ws.windows().len(), 2);
        // Move the detached window's only tab back into the main window.
        ws.move_tab(PanelInstanceId(1), main, PanelInstanceId(0))
            .unwrap();
        assert_eq!(ws.windows().len(), 1);
        assert!(ws.window(detached).is_none());
        assert!(ws.is_valid());
    }

    #[test]
    fn move_tab_rejects_unknown_targets_and_main_last_tab() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        assert_eq!(
            ws.move_tab(PanelInstanceId(42), main, PanelInstanceId(0)),
            Err(LayoutError::UnknownInstance(PanelInstanceId(42)))
        );
        assert_eq!(
            ws.move_tab(PanelInstanceId(0), WindowId(99), PanelInstanceId(0)),
            Err(LayoutError::UnknownWindow(WindowId(99)))
        );
        assert_eq!(
            ws.move_tab(PanelInstanceId(0), main, PanelInstanceId(42)),
            Err(LayoutError::UnknownInstance(PanelInstanceId(42)))
        );

        let mut single = WorkspaceLayout::new(area1(0, Viewer)).unwrap();
        let main = single.main_window().id;
        // Moving the main window's only tab anywhere is rejected...
        assert_eq!(
            single.move_tab(PanelInstanceId(0), main, PanelInstanceId(0)),
            Ok(()),
            "moving a tab onto itself is a no-op"
        );
        // ...but moving it out to another area would empty the main window.
        let mut ws = workspace();
        let main = ws.main_window().id;
        // Drain everything else first so #0 is alone.
        let d = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        ws.close_window(d).unwrap();
        let d = ws.detach_to_window(PanelInstanceId(3)).unwrap();
        ws.close_window(d).unwrap();
        let d = ws.detach_to_window(PanelInstanceId(2)).unwrap();
        ws.close_window(d).unwrap();
        assert_eq!(ws.main_window().root.instances(), vec![inst(0, Viewer)]);
        assert_eq!(
            ws.detach_to_window(PanelInstanceId(0)),
            Err(LayoutError::MainWindowLastArea)
        );
        assert!(ws.window(main).is_some());
    }

    // -- detach / close_window -----------------------------------------------

    #[test]
    fn detach_to_window_creates_single_area_window() {
        let mut ws = workspace();
        let id = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        assert!(ws.is_valid());
        let window = ws.window(id).unwrap();
        assert_eq!(window.root.instances(), vec![inst(1, Timeline)]);
        assert!(window.placement.is_none());
        assert!(!window.always_on_top);
        // Source area kept NodeGraph and is still in the main window.
        assert_eq!(ws.windows().len(), 2);
        assert!(ws.main_window().root.contains(PanelInstanceId(2)));
        // Window ids increase monotonically.
        let id2 = ws.detach_to_window(PanelInstanceId(2)).unwrap();
        assert!(id2 > id);
    }

    /// The always-on-top pin is per window: flipping one window's flag leaves
    /// every other window (the main window included) alone.
    #[test]
    fn always_on_top_is_owned_by_each_window() {
        let mut ws = workspace();
        let first = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        let second = ws.detach_to_window(PanelInstanceId(2)).unwrap();
        ws.window_mut(first).unwrap().always_on_top = true;
        assert!(ws.window(first).unwrap().always_on_top);
        assert!(!ws.window(second).unwrap().always_on_top);
        assert!(!ws.main_window().always_on_top);
        assert!(ws.is_valid());
    }

    #[test]
    fn detach_source_split_folds_when_area_empties() {
        let mut ws = workspace();
        ws.detach_to_window(PanelInstanceId(0)).unwrap();
        assert_eq!(ws.main_window().root.area_count(), 2);
        assert!(ws.is_valid());
    }

    #[test]
    fn close_window_rules() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        assert_eq!(ws.close_window(main), Err(LayoutError::MainWindowClose));
        assert_eq!(
            ws.close_window(WindowId(99)),
            Err(LayoutError::UnknownWindow(WindowId(99)))
        );
        let detached = ws.detach_to_window(PanelInstanceId(0)).unwrap();
        ws.close_window(detached).unwrap();
        assert!(ws.window(detached).is_none());
        assert!(ws.is_valid());
    }

    // -- duplicate_instance ----------------------------------------------------

    #[test]
    fn duplicate_instance_inserts_after_source_and_activates() {
        let mut ws = workspace();
        let new_id = ws.duplicate_instance(PanelInstanceId(1)).unwrap();
        assert!(ws.is_valid());
        assert_ne!(new_id, PanelInstanceId(1));
        let (window_id, new_instance) = ws.find_instance(new_id).unwrap();
        assert_eq!(window_id, ws.main_window().id);
        assert_eq!(new_instance.kind, Timeline);
        // Order inside the middle area: Timeline#1, Timeline#new, NodeGraph#2.
        let root = &ws.main_window().root;
        let ids: Vec<_> = root.instances().iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            vec![
                PanelInstanceId(0),
                PanelInstanceId(1),
                new_id,
                PanelInstanceId(2),
                PanelInstanceId(3)
            ]
        );
    }

    #[test]
    fn duplicate_unknown_instance_is_rejected() {
        let mut ws = workspace();
        assert_eq!(
            ws.duplicate_instance(PanelInstanceId(42)),
            Err(LayoutError::UnknownInstance(PanelInstanceId(42)))
        );
    }

    #[test]
    fn duplicate_then_split_separates_instances() {
        // The "duplicate and split" area-menu action is these two ops composed.
        let mut ws = workspace();
        let main = ws.main_window().id;
        let new_id = ws.duplicate_instance(PanelInstanceId(1)).unwrap();
        ws.split(main, new_id, Horizontal, 0.5).unwrap();
        assert!(ws.is_valid());
        assert_eq!(ws.main_window().root.area_count(), 4);
    }

    // -- insert_instance -------------------------------------------------------

    #[test]
    fn insert_instance_adopts_edge_area_tab_and_activates() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        // MediaBin docks left: the leftmost area is the Viewer area (#0).
        let id = ws.insert_instance(main, MediaBin).unwrap();
        assert!(ws.is_valid());
        assert_eq!(id, PanelInstanceId(4), "id comes from the counter");
        let root = &ws.main_window().root;
        assert_eq!(root.area_count(), 3, "no new area is created");
        let LayoutNode::Split { first, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Area { tabs, active } = first.as_ref() else {
            panic!("left edge should still be an area");
        };
        assert_eq!(tabs, &vec![inst(0, Viewer), inst(4, MediaBin)]);
        assert_eq!(*active, 1, "the inserted tab is active");

        // Properties docks right: descent takes second, then the larger child
        // of the vertical split (0.7) — the middle Timeline/NodeGraph area.
        let id = ws.insert_instance(main, Properties).unwrap();
        let root = &ws.main_window().root;
        let LayoutNode::Split { second, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Split { first: middle, .. } = second.as_ref() else {
            panic!("right subtree should still be a split");
        };
        let LayoutNode::Area { tabs, active } = middle.as_ref() else {
            panic!("middle should still be an area");
        };
        assert_eq!(
            tabs,
            &vec![inst(1, Timeline), inst(2, NodeGraph), inst(5, Properties)]
        );
        assert_eq!(*active, 2);
        assert_eq!(id, PanelInstanceId(5));
    }

    #[test]
    fn insert_instance_bottom_and_center_follow_the_larger_child() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        // Bottom at the horizontal root takes the larger child (0.6: Viewer),
        // so RenderQueue tabs with the Viewer.
        ws.insert_instance(main, RenderQueue).unwrap();
        assert!(ws.is_valid());
        let root = &ws.main_window().root;
        let LayoutNode::Split { first, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Area { tabs, .. } = first.as_ref() else {
            panic!("left edge should still be an area");
        };
        assert_eq!(tabs, &vec![inst(0, Viewer), inst(4, RenderQueue)]);

        // Center always follows the larger child: root 0.6 first, then the
        // Viewer area again (it is a leaf at this point).
        let id = ws.insert_instance(main, ShaderEditor).unwrap();
        let root = &ws.main_window().root;
        let LayoutNode::Split { first, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Area { tabs, .. } = first.as_ref() else {
            panic!("left edge should still be an area");
        };
        assert_eq!(
            tabs,
            &vec![inst(0, Viewer), inst(4, RenderQueue), inst(5, ShaderEditor)]
        );
        assert_eq!(id, PanelInstanceId(5));
    }

    #[test]
    fn insert_instance_rejects_unknown_window() {
        let mut ws = workspace();
        assert_eq!(
            ws.insert_instance(WindowId(99), Viewer),
            Err(LayoutError::UnknownWindow(WindowId(99)))
        );
    }

    // -- activate_tab ----------------------------------------------------------

    #[test]
    fn activate_tab_selects_the_tab_in_its_area() {
        let mut ws = workspace();
        ws.activate_tab(PanelInstanceId(1)).unwrap();
        let root = &ws.main_window().root;
        let LayoutNode::Split { second, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Split { first: middle, .. } = second.as_ref() else {
            panic!("right subtree should still be a split");
        };
        let LayoutNode::Area { active, .. } = middle.as_ref() else {
            panic!("middle should still be an area");
        };
        assert_eq!(*active, 0);
        assert_eq!(
            ws.activate_tab(PanelInstanceId(42)),
            Err(LayoutError::UnknownInstance(PanelInstanceId(42)))
        );
    }

    // -- remove_instance -------------------------------------------------------

    #[test]
    fn remove_instance_drops_tab_and_keeps_area() {
        let mut ws = workspace();
        let removed = ws.remove_instance(PanelInstanceId(2)).unwrap();
        assert_eq!(removed, inst(2, NodeGraph));
        assert!(ws.is_valid());
        let root = &ws.main_window().root;
        assert_eq!(root.area_count(), 3);
        // The middle area was active on NodeGraph (index 1); it falls back to
        // the last remaining tab.
        let LayoutNode::Split { second, .. } = root else {
            panic!("root should still be a split");
        };
        let LayoutNode::Split { first: middle, .. } = second.as_ref() else {
            panic!("right subtree should still be a split");
        };
        let LayoutNode::Area { tabs, active } = middle.as_ref() else {
            panic!("middle should still be an area");
        };
        assert_eq!(tabs, &vec![inst(1, Timeline)]);
        assert_eq!(*active, 0);
    }

    #[test]
    fn remove_instance_folds_split_when_area_empties() {
        let mut ws = workspace();
        ws.remove_instance(PanelInstanceId(0)).unwrap();
        assert!(ws.is_valid());
        let root = &ws.main_window().root;
        assert_eq!(root.area_count(), 2);
        assert_eq!(
            root.instances(),
            vec![inst(1, Timeline), inst(2, NodeGraph), inst(3, Outliner)]
        );
    }

    #[test]
    fn remove_instance_rejects_main_last_tab_and_unknown() {
        let mut ws = WorkspaceLayout::new(area1(0, Viewer)).unwrap();
        assert_eq!(
            ws.remove_instance(PanelInstanceId(0)),
            Err(LayoutError::MainWindowLastArea)
        );
        assert_eq!(
            ws.remove_instance(PanelInstanceId(42)),
            Err(LayoutError::UnknownInstance(PanelInstanceId(42)))
        );
        assert_eq!(ws.main_window().root, area1(0, Viewer));
    }

    #[test]
    fn remove_instance_closes_emptied_detached_window() {
        let mut ws = workspace();
        let detached = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        ws.remove_instance(PanelInstanceId(1)).unwrap();
        assert!(ws.window(detached).is_none());
        assert_eq!(ws.windows().len(), 1);
        assert!(ws.is_valid());
    }

    // -- replace_main_tree -----------------------------------------------------

    #[test]
    fn replace_main_tree_renumbers_ids_around_detached_windows() {
        let mut ws = workspace();
        let detached = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        // A preset-like tree with deterministic 0-based ids that would
        // collide with the detached instance (#1) if adopted raw.
        let preset = LayoutNode::split(Horizontal, 0.5, area1(0, Viewer), area1(1, Properties));
        ws.replace_main_tree(preset).unwrap();
        assert!(ws.is_valid());
        // The detached window is untouched...
        assert_eq!(
            ws.window(detached).unwrap().root.instances(),
            vec![inst(1, Timeline)]
        );
        // ...and the new main tree was renumbered above every live id.
        let ids: Vec<_> = ws
            .main_window()
            .root
            .instances()
            .iter()
            .map(|t| (t.id, t.kind))
            .collect();
        assert_eq!(
            ids,
            vec![
                (PanelInstanceId(4), Viewer),
                (PanelInstanceId(5), Properties)
            ]
        );
    }

    #[test]
    fn replace_main_tree_rejects_invalid_tree_without_mutating() {
        let mut ws = workspace();
        let before = ws.clone();
        let bad = LayoutNode::split(Horizontal, 1.5, area1(0, Viewer), area1(1, Timeline));
        assert_eq!(
            ws.replace_main_tree(bad),
            Err(LayoutValidationError::InvalidTree(ws.main_window().id))
        );
        assert_eq!(ws, before);
    }

    // -- absorb_window ---------------------------------------------------------

    #[test]
    fn absorb_window_moves_every_instance_to_main_and_closes() {
        let mut ws = workspace();
        let first = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        let second = ws.detach_to_window(PanelInstanceId(2)).unwrap();
        assert_eq!(ws.windows().len(), 3);

        let moved = ws.absorb_window(first).unwrap();
        assert_eq!(moved, vec![inst(1, Timeline)]);
        assert!(ws.window(first).is_none());
        assert!(ws.window(second).is_some());
        assert!(ws.is_valid());
        // Timeline (bottom slot) landed in the main window, id preserved.
        let (window, instance) = ws.find_instance(PanelInstanceId(1)).unwrap();
        assert_eq!(window, ws.main_window().id);
        assert_eq!(instance.kind, Timeline);

        let moved = ws.absorb_window(second).unwrap();
        assert_eq!(moved, vec![inst(2, NodeGraph)]);
        assert_eq!(ws.windows().len(), 1);
        assert!(ws.is_valid());
    }

    #[test]
    fn absorb_window_with_multiple_areas_preserves_ids_and_slots() {
        let mut ws = workspace();
        let detached = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        // Give the detached window a second tab and split it into two areas.
        ws.move_tab(PanelInstanceId(2), detached, PanelInstanceId(1))
            .unwrap();
        ws.split(detached, PanelInstanceId(2), Horizontal, 0.5)
            .unwrap();
        assert_eq!(ws.window(detached).unwrap().root.area_count(), 2);
        assert!(ws.is_valid());

        let moved = ws.absorb_window(detached).unwrap();
        assert_eq!(moved, vec![inst(1, Timeline), inst(2, NodeGraph)]);
        assert!(ws.window(detached).is_none());
        assert!(ws.is_valid());
        // Every instance came back to the main window with its id preserved:
        // Timeline to its bottom slot, NodeGraph to its center slot.
        for (id, kind) in [(1, Timeline), (2, NodeGraph)] {
            let (window, instance) = ws.find_instance(PanelInstanceId(id)).unwrap();
            assert_eq!(window, ws.main_window().id);
            assert_eq!(instance.kind, kind);
        }
    }

    #[test]
    fn absorb_window_rejects_main_and_unknown_window() {
        let mut ws = workspace();
        let main = ws.main_window().id;
        assert_eq!(ws.absorb_window(main), Err(LayoutError::MainWindowClose));
        assert_eq!(
            ws.absorb_window(WindowId(99)),
            Err(LayoutError::UnknownWindow(WindowId(99)))
        );
    }

    // -- validation on construction and deserialization ------------------------

    fn window(id: u64, root: LayoutNode) -> WindowLayout {
        WindowLayout {
            id: WindowId(id),
            root,
            placement: None,
            always_on_top: false,
        }
    }

    /// Invalid workspaces and the violation each one exhibits.
    fn invalid_layouts() -> Vec<WorkspaceLayout> {
        vec![
            // No windows at all: windows[0] would not exist.
            WorkspaceLayout {
                windows: vec![],
                next_window_id: 1,
                next_instance_id: 0,
            },
            // Two windows sharing an id.
            WorkspaceLayout {
                windows: vec![window(0, area1(0, Viewer)), window(0, area1(1, Timeline))],
                next_window_id: 1,
                next_instance_id: 2,
            },
            // The same instance id in two different windows.
            WorkspaceLayout {
                windows: vec![window(0, area1(0, Viewer)), window(1, area1(0, Timeline))],
                next_window_id: 2,
                next_instance_id: 1,
            },
            // An invalid tree (empty area).
            WorkspaceLayout {
                windows: vec![window(0, LayoutNode::area(vec![]))],
                next_window_id: 1,
                next_instance_id: 0,
            },
            // next_window_id not ahead of the window ids in use.
            WorkspaceLayout {
                windows: vec![window(0, area1(0, Viewer)), window(1, area1(1, Timeline))],
                next_window_id: 1,
                next_instance_id: 2,
            },
            // next_instance_id not ahead of the instance ids in use.
            WorkspaceLayout {
                windows: vec![window(0, area1(5, Viewer))],
                next_window_id: 1,
                next_instance_id: 5,
            },
        ]
    }

    #[test]
    fn new_rejects_invalid_tree() {
        assert_eq!(
            WorkspaceLayout::new(LayoutNode::area(vec![])),
            Err(LayoutValidationError::InvalidTree(WindowId(0)))
        );
        let bad_ratio = LayoutNode::split(Horizontal, 0.0, area1(0, Viewer), area1(1, Timeline));
        assert_eq!(
            WorkspaceLayout::new(bad_ratio),
            Err(LayoutValidationError::InvalidTree(WindowId(0)))
        );
    }

    #[test]
    fn validate_reports_each_violation() {
        assert!(workspace().validate().is_ok());
        let expected = [
            LayoutValidationError::NoWindows,
            LayoutValidationError::DuplicateWindowId(WindowId(0)),
            LayoutValidationError::DuplicateInstanceId(PanelInstanceId(0)),
            LayoutValidationError::InvalidTree(WindowId(0)),
            LayoutValidationError::StaleWindowIdCounter(WindowId(1)),
            LayoutValidationError::StaleInstanceIdCounter(PanelInstanceId(5)),
        ];
        for (layout, expected) in invalid_layouts().into_iter().zip(expected) {
            assert_eq!(layout.validate(), Err(expected.clone()), "{expected}");
            assert!(!layout.is_valid());
        }
    }

    #[test]
    fn deserialization_rejects_invalid_layouts() {
        for layout in invalid_layouts() {
            let err = layout.validate().unwrap_err();
            let json = serde_json::to_string(&layout).unwrap();
            assert!(
                serde_json::from_str::<WorkspaceLayout>(&json).is_err(),
                "JSON must reject {err}"
            );
            let toml = toml::to_string_pretty(&layout).unwrap();
            assert!(
                toml::from_str::<WorkspaceLayout>(&toml).is_err(),
                "TOML must reject {err}"
            );
        }
    }
}

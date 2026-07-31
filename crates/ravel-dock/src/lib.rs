// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Docking UI for Ravel layout trees.
//!
//! ravel-dock renders one window's [`ravel_ui::layout::LayoutNode`] tree as a
//! GPUI element tree: split containers with draggable separators, tab bars
//! for areas, and an empty-area placeholder. It is a pure view crate — it
//! depends on `ravel-ui` only for the headless layout model types and treats
//! every panel instance as opaque. The host (`ravel-app`, or the bundled
//! `gallery` example) owns the model and the pane contents:
//!
//! - Pane contents are supplied through the [`PaneContent`] trait.
//! - User interactions are emitted as [`DockEvent`]s. ravel-dock never writes
//!   the model itself; the host applies events to its own layout state and
//!   pushes the updated tree back with [`DockRoot::set_layout`]. The helpers
//!   in [`path`] cover the built-in event kinds.

pub mod content;
pub mod dock;
pub mod layout_math;
pub mod path;

pub use content::PaneContent;
pub use dock::{AreaAction, DockEvent, DockRoot};
pub use layout_math::DropZone;
pub use path::{
    NodePath, SplitSide, activate_tab, apply_area_action, apply_tab_drop, lead_split_child,
    set_ratio_at, tab_drop_changes_layout,
};

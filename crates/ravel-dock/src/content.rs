// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The interface through which the host supplies pane contents.

use gpui::{AnyElement, AnyView, App, SharedString, Window};
use ravel_ui::layout::PanelInstance;

/// Supplies the contents of docked panes.
///
/// ravel-dock owns the frame (split tree, tab bars, splitters); the host owns
/// what lives inside each pane. Implementations receive the opaque
/// [`PanelInstance`] — ravel-dock itself never branches on the panel kind.
pub trait PaneContent {
    /// The tab-bar label for one panel instance.
    fn tab_title(&self, instance: &PanelInstance, window: &Window, cx: &App) -> SharedString;

    /// The view rendered for the active tab of an area.
    ///
    /// Implementations must return a stable view per instance id (cache the
    /// underlying entity): creating a fresh entity per render call would
    /// reset the pane's view state on every frame.
    fn view(&self, instance: &PanelInstance, window: &mut Window, cx: &mut App) -> AnyView;

    /// What to render for an area without tabs. Valid trees never contain
    /// one, so this only shows for transient or externally constructed
    /// states. `None` renders ravel-dock's minimal default placeholder.
    fn empty_state(&self, _window: &mut Window, _cx: &mut App) -> Option<AnyElement> {
        None
    }
}

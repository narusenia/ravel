// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Multi-window / panel-detach state management.
//!
//! Panels can be detached from the main window into their own OS window (for
//! multi-monitor setups) and reattached when that window closes. This module
//! owns the bookkeeping — which panels are currently detached, into which
//! window, and where on the desktop — so it can be restored across sessions.
//! Spawning the actual GPUI windows is the host's responsibility; it drives
//! this state machine.

use crate::panel::PanelKind;
use serde::{Deserialize, Serialize};

/// Opaque identifier for a detached window, unique within a [`WindowManager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId(pub u64);

/// On-desktop placement of a detached window, in logical pixels.
///
/// Recorded so multi-monitor arrangements can be restored on next launch.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowPlacement {
    /// X position of the top-left corner.
    pub x: f32,
    /// Y position of the top-left corner.
    pub y: f32,
    /// Window width.
    pub width: f32,
    /// Window height.
    pub height: f32,
}

/// A panel that has been detached into its own window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetachedWindow {
    /// The window's identifier.
    pub id: WindowId,
    /// The panel hosted in the detached window.
    pub panel: PanelKind,
    /// Last known placement, if recorded.
    pub placement: Option<WindowPlacement>,
}

/// Error returned by detach/reattach operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WindowError {
    /// The panel is already detached.
    #[error("panel {0:?} is already detached")]
    AlreadyDetached(PanelKind),
    /// No detached window has the given id.
    #[error("no detached window with id {0:?}")]
    UnknownWindow(WindowId),
}

/// Tracks the set of detached panel windows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WindowManager {
    detached: Vec<DetachedWindow>,
    next_id: u64,
}

impl WindowManager {
    /// Creates an empty manager (no detached windows).
    pub fn new() -> Self {
        Self::default()
    }

    /// Detaches `panel` into a new window, returning its id.
    ///
    /// Fails if the panel is already detached.
    pub fn detach(&mut self, panel: PanelKind) -> Result<WindowId, WindowError> {
        if self.is_detached(panel) {
            return Err(WindowError::AlreadyDetached(panel));
        }
        let id = WindowId(self.next_id);
        self.next_id += 1;
        self.detached.push(DetachedWindow {
            id,
            panel,
            placement: None,
        });
        Ok(id)
    }

    /// Reattaches the window with `id`, returning the panel that returns to the
    /// main window.
    ///
    /// Fails if no such window exists.
    pub fn reattach(&mut self, id: WindowId) -> Result<PanelKind, WindowError> {
        let pos = self
            .detached
            .iter()
            .position(|w| w.id == id)
            .ok_or(WindowError::UnknownWindow(id))?;
        Ok(self.detached.remove(pos).panel)
    }

    /// Records the latest placement for a detached window (for restore).
    pub fn set_placement(
        &mut self,
        id: WindowId,
        placement: WindowPlacement,
    ) -> Result<(), WindowError> {
        let window = self
            .detached
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or(WindowError::UnknownWindow(id))?;
        window.placement = Some(placement);
        Ok(())
    }

    /// Returns `true` if `panel` is currently detached.
    pub fn is_detached(&self, panel: PanelKind) -> bool {
        self.detached.iter().any(|w| w.panel == panel)
    }

    /// Looks up the window id hosting `panel`, if detached.
    pub fn window_of(&self, panel: PanelKind) -> Option<WindowId> {
        self.detached
            .iter()
            .find(|w| w.panel == panel)
            .map(|w| w.id)
    }

    /// Iterates over all detached windows.
    pub fn detached(&self) -> impl Iterator<Item = &DetachedWindow> {
        self.detached.iter()
    }

    /// Number of detached windows.
    pub fn len(&self) -> usize {
        self.detached.len()
    }

    /// Returns `true` if no panels are detached.
    pub fn is_empty(&self) -> bool {
        self.detached.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_then_reattach_roundtrips() {
        let mut wm = WindowManager::new();
        assert!(wm.is_empty());
        let id = wm.detach(PanelKind::Viewer).unwrap();
        assert!(wm.is_detached(PanelKind::Viewer));
        assert_eq!(wm.window_of(PanelKind::Viewer), Some(id));
        assert_eq!(wm.len(), 1);

        let panel = wm.reattach(id).unwrap();
        assert_eq!(panel, PanelKind::Viewer);
        assert!(!wm.is_detached(PanelKind::Viewer));
        assert!(wm.is_empty());
    }

    #[test]
    fn detaching_twice_is_rejected() {
        let mut wm = WindowManager::new();
        wm.detach(PanelKind::Viewer).unwrap();
        let err = wm.detach(PanelKind::Viewer).unwrap_err();
        assert_eq!(err, WindowError::AlreadyDetached(PanelKind::Viewer));
    }

    #[test]
    fn reattaching_unknown_window_is_rejected() {
        let mut wm = WindowManager::new();
        let err = wm.reattach(WindowId(99)).unwrap_err();
        assert_eq!(err, WindowError::UnknownWindow(WindowId(99)));
    }

    #[test]
    fn window_ids_are_unique_across_detaches() {
        let mut wm = WindowManager::new();
        let a = wm.detach(PanelKind::Viewer).unwrap();
        let b = wm.detach(PanelKind::NodeGraph).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn placement_is_recorded_and_serializes() {
        let mut wm = WindowManager::new();
        let id = wm.detach(PanelKind::Viewer).unwrap();
        let placement = WindowPlacement {
            x: 100.0,
            y: 200.0,
            width: 1280.0,
            height: 720.0,
        };
        wm.set_placement(id, placement).unwrap();

        let json = serde_json::to_string(&wm).unwrap();
        let restored: WindowManager = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.detached().next().unwrap().placement,
            Some(placement)
        );
    }
}

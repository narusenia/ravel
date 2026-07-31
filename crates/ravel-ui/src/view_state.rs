// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-instance view state storage.
//!
//! Multiple instances of the same panel kind share the document model
//! (project, selection, active composition, playhead) but each keeps its own
//! view state — zoom, pan, scroll position, display target. This module
//! provides the keyed container for that state: one entry per
//! [`PanelInstanceId`], prunable against the live [`WorkspaceLayout`] so
//! state for destroyed instances does not linger.

use crate::layout::{PanelInstanceId, WorkspaceLayout};
use std::collections::HashMap;

/// View state keyed by panel instance.
///
/// The state type `T` is chosen by the owner (the host keeps one store per
/// stateful panel family); this container only handles keying and lifecycle.
#[derive(Debug, Clone)]
pub struct ViewStates<T> {
    states: HashMap<PanelInstanceId, T>,
}

impl<T> Default for ViewStates<T> {
    fn default() -> Self {
        Self {
            states: HashMap::new(),
        }
    }
}

impl<T> ViewStates<T> {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the state stored for `id`, if any.
    pub fn get(&self, id: PanelInstanceId) -> Option<&T> {
        self.states.get(&id)
    }

    /// Mutable access to the state stored for `id`, if any.
    pub fn get_mut(&mut self, id: PanelInstanceId) -> Option<&mut T> {
        self.states.get_mut(&id)
    }

    /// Stores `state` for `id`, returning the previous entry if there was one.
    pub fn insert(&mut self, id: PanelInstanceId, state: T) -> Option<T> {
        self.states.insert(id, state)
    }

    /// Removes and returns the state stored for `id`, if any.
    pub fn remove(&mut self, id: PanelInstanceId) -> Option<T> {
        self.states.remove(&id)
    }

    /// Returns `true` if `id` has stored state.
    pub fn contains(&self, id: PanelInstanceId) -> bool {
        self.states.contains_key(&id)
    }

    /// Number of instances with stored state.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Returns `true` if no state is stored.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Drops state for every instance that no longer exists in `layout`
    /// (closed areas, closed windows, preset replacement). Call after layout
    /// mutations so stale entries cannot accumulate.
    pub fn retain_instances(&mut self, layout: &WorkspaceLayout) {
        self.states
            .retain(|id, _| layout.find_instance(*id).is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LayoutNode;
    use crate::panel::PanelKind;

    fn workspace() -> WorkspaceLayout {
        let root = LayoutNode::split(
            crate::layout::Orientation::Horizontal,
            0.5,
            LayoutNode::area(vec![crate::layout::PanelInstance::new(
                PanelInstanceId(0),
                PanelKind::Viewer,
            )]),
            LayoutNode::area(vec![crate::layout::PanelInstance::new(
                PanelInstanceId(1),
                PanelKind::Timeline,
            )]),
        );
        WorkspaceLayout::new(root).unwrap()
    }

    #[test]
    fn insert_get_remove_roundtrip() {
        let mut states = ViewStates::new();
        assert!(states.is_empty());
        assert_eq!(states.insert(PanelInstanceId(0), 1.5_f32), None);
        assert_eq!(states.insert(PanelInstanceId(0), 2.0), Some(1.5));
        assert_eq!(states.get(PanelInstanceId(0)), Some(&2.0));
        assert!(states.contains(PanelInstanceId(0)));
        assert!(!states.contains(PanelInstanceId(1)));

        *states.get_mut(PanelInstanceId(0)).unwrap() = 3.0;
        assert_eq!(states.get(PanelInstanceId(0)), Some(&3.0));

        assert_eq!(states.remove(PanelInstanceId(0)), Some(3.0));
        assert!(states.is_empty());
    }

    #[test]
    fn retain_instances_drops_state_of_gone_instances() {
        let mut ws = workspace();
        let mut states = ViewStates::new();
        states.insert(PanelInstanceId(0), "viewer");
        states.insert(PanelInstanceId(1), "timeline");
        // A stale entry with no matching instance.
        states.insert(PanelInstanceId(42), "ghost");

        // Detaching keeps the instance alive across windows.
        let detached = ws.detach_to_window(PanelInstanceId(1)).unwrap();
        states.retain_instances(&ws);
        assert_eq!(states.len(), 2);
        assert!(states.contains(PanelInstanceId(1)));

        // Closing the detached window destroys the instance.
        ws.close_window(detached).unwrap();
        states.retain_instances(&ws);
        assert_eq!(states.len(), 1);
        assert_eq!(states.get(PanelInstanceId(0)), Some(&"viewer"));
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Modifier-driven layer selection arithmetic, shared by the Timeline and the
//! Outliner (REQ-UI-013).
//!
//! Both panels write one shared selection, so a modified click has to mean the
//! same thing in both. The meaning is computed here, headless: a panel supplies
//! the current selection, the composition's stack order, and the clicked layer,
//! and writes the returned list as the new selection.
//!
//! The returned order is the selection order the panels keep: the **anchor
//! first**. The anchor is the layer a range extends from, so a repeated
//! Shift-click grows and shrinks around the layer the user started at instead
//! of walking away from it.

use ravel_core::id::LayerId;

/// What a click on a layer row means, decided by the held modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerClickMode {
    /// Plain click: the clicked layer becomes the whole selection.
    #[default]
    Replace,
    /// Platform modifier (Cmd / Win / Super): add the clicked layer, or drop it
    /// when it is already selected.
    Toggle,
    /// Shift: select every layer between the anchor and the clicked layer.
    Range,
}

impl LayerClickMode {
    /// Shift wins when both modifiers are held: a range is the more specific
    /// request, and Shift+Cmd is not its own gesture.
    pub fn from_modifiers(shift: bool, platform: bool) -> Self {
        if shift {
            Self::Range
        } else if platform {
            Self::Toggle
        } else {
            Self::Replace
        }
    }

    /// Whether this click extends an existing selection. A drag gesture starts
    /// only on a plain click — otherwise building a selection by Shift- or
    /// Cmd-clicking rows would reorder or move layers by accident.
    pub fn is_additive(self) -> bool {
        !matches!(self, Self::Replace)
    }
}

/// The selection after clicking `clicked` with `mode` held.
///
/// `current` is the selection being modified (anchor first) and `order` is the
/// composition's layer stack order, which is what a range spans. A range with
/// no anchor, or one naming a layer that is not in `order` (a stale selection,
/// a row of another composition), degrades to [`LayerClickMode::Replace`]
/// rather than selecting nothing.
pub fn layer_selection_after_click(
    current: &[LayerId],
    order: &[LayerId],
    clicked: LayerId,
    mode: LayerClickMode,
) -> Vec<LayerId> {
    match mode {
        LayerClickMode::Replace => vec![clicked],
        LayerClickMode::Toggle => {
            if current.contains(&clicked) {
                current
                    .iter()
                    .copied()
                    .filter(|id| *id != clicked)
                    .collect()
            } else {
                // The newly clicked layer becomes the anchor, so a following
                // Shift-click ranges from what was just added.
                let mut next = vec![clicked];
                next.extend(current.iter().copied());
                next
            }
        }
        LayerClickMode::Range => {
            let anchor = current.first().copied();
            let span = anchor
                .and_then(|anchor| {
                    let from = order.iter().position(|id| *id == anchor)?;
                    let to = order.iter().position(|id| *id == clicked)?;
                    Some((anchor, from.min(to), from.max(to)))
                })
                .map(|(anchor, from, to)| {
                    let mut next = vec![anchor];
                    next.extend(order[from..=to].iter().copied().filter(|id| *id != anchor));
                    next
                });
            span.unwrap_or_else(|| vec![clicked])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(count: usize) -> Vec<LayerId> {
        (0..count).map(|_| LayerId::next()).collect()
    }

    #[test]
    fn modifiers_map_to_modes() {
        assert_eq!(
            LayerClickMode::from_modifiers(false, false),
            LayerClickMode::Replace
        );
        assert_eq!(
            LayerClickMode::from_modifiers(false, true),
            LayerClickMode::Toggle
        );
        assert_eq!(
            LayerClickMode::from_modifiers(true, false),
            LayerClickMode::Range
        );
        assert_eq!(
            LayerClickMode::from_modifiers(true, true),
            LayerClickMode::Range,
            "shift is the more specific request"
        );
        assert!(!LayerClickMode::Replace.is_additive());
        assert!(LayerClickMode::Toggle.is_additive());
        assert!(LayerClickMode::Range.is_additive());
    }

    #[test]
    fn a_plain_click_replaces_the_selection() {
        let order = ids(3);
        let current = vec![order[0], order[2]];
        assert_eq!(
            layer_selection_after_click(&current, &order, order[1], LayerClickMode::Replace),
            vec![order[1]]
        );
    }

    #[test]
    fn a_toggle_click_adds_the_layer_as_the_new_anchor() {
        let order = ids(3);
        let current = vec![order[0]];
        let next = layer_selection_after_click(&current, &order, order[2], LayerClickMode::Toggle);
        assert_eq!(
            next,
            vec![order[2], order[0]],
            "the clicked layer becomes the anchor a following range extends from"
        );
    }

    #[test]
    fn a_toggle_click_removes_an_already_selected_layer() {
        let order = ids(3);
        let current = vec![order[2], order[0], order[1]];
        assert_eq!(
            layer_selection_after_click(&current, &order, order[0], LayerClickMode::Toggle),
            vec![order[2], order[1]],
            "the remaining layers keep their selection order"
        );
        assert!(
            layer_selection_after_click(&[order[0]], &order, order[0], LayerClickMode::Toggle)
                .is_empty(),
            "deselecting the last layer is allowed — an empty selection is a state"
        );
    }

    #[test]
    fn a_range_click_spans_the_stack_in_both_directions() {
        let order = ids(4);
        let forward =
            layer_selection_after_click(&[order[1]], &order, order[3], LayerClickMode::Range);
        assert_eq!(forward, vec![order[1], order[2], order[3]]);

        let backward =
            layer_selection_after_click(&[order[2]], &order, order[0], LayerClickMode::Range);
        assert_eq!(
            backward,
            vec![order[2], order[0], order[1]],
            "the anchor stays first; the span follows the stack order"
        );
    }

    #[test]
    fn repeated_range_clicks_extend_from_the_same_anchor() {
        let order = ids(4);
        let first =
            layer_selection_after_click(&[order[0]], &order, order[2], LayerClickMode::Range);
        assert_eq!(first, vec![order[0], order[1], order[2]]);
        // Shrinking back has to reach the anchor alone, not the previous span.
        let second = layer_selection_after_click(&first, &order, order[0], LayerClickMode::Range);
        assert_eq!(second, vec![order[0]]);
        let third = layer_selection_after_click(&second, &order, order[3], LayerClickMode::Range);
        assert_eq!(third, vec![order[0], order[1], order[2], order[3]]);
    }

    #[test]
    fn a_range_without_a_usable_anchor_falls_back_to_replace() {
        let order = ids(3);
        assert_eq!(
            layer_selection_after_click(&[], &order, order[1], LayerClickMode::Range),
            vec![order[1]],
            "nothing selected yet"
        );
        let foreign = LayerId::next();
        assert_eq!(
            layer_selection_after_click(&[foreign], &order, order[1], LayerClickMode::Range),
            vec![order[1]],
            "an anchor that is not in this stack"
        );
    }
}

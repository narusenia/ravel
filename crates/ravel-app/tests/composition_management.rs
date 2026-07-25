// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition creation, settings, duplication, and deletion (REQ-UI-013,
//! unit 4).
//!
//! These pin the document-level guarantees the dialogs and the Outliner rely
//! on: each operation is exactly one undo step, the active composition follows
//! the operation, and composition 0 is a state the app can both reach and leave.
//!
//! They live in an integration test rather than a `mod tests` inside
//! `panels/mod.rs` because rustc overflows its stack while compiling this
//! crate's lib-test target with a test module in that file.

use gpui::{AppContext as _, TestAppContext};
use ravel_app::panels::{self, PropertiesTarget, SelectedPropertiesTarget};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::Layer;
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::{Color, FrameRate};
use ravel_ui::document::{CompositionSettings, add_layer};

fn project(cx: &mut TestAppContext) -> gpui::Entity<ProjectState> {
    ravel_app::project_state::disable_background_eval_for_tests();
    cx.update(|cx| {
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        cx.set_global(SelectedPropertiesTarget::default());
        project
    })
}

fn settings(name: &str) -> CompositionSettings {
    CompositionSettings {
        name: name.to_string(),
        resolution: (1280, 720),
        frame_rate: FrameRate::new(24, 1),
        duration_frames: 120,
        background_color: Color::new(0.1, 0.2, 0.3, 1.0),
    }
}

/// A created composition is the one the UI switches to, and one undo removes it
/// again (the dialog holds unconfirmed values, so nothing is created twice).
#[gpui::test]
fn creating_a_composition_activates_it_and_is_one_undo_step(cx: &mut TestAppContext) {
    let project = project(cx);
    let root = project.read_with(cx, |project, _| project.document().root_comp.unwrap());

    let created = project.update(cx, |project, cx| {
        project.create_composition(settings("Shot 1"), cx)
    });

    project.read_with(cx, |project, cx| {
        assert_eq!(panels::active_composition(cx), Some(created));
        let comp = project.document().get_composition(created).unwrap();
        assert_eq!(comp.name, "Shot 1");
        assert_eq!(comp.resolution, (1280, 720));
        assert_eq!(comp.frame_rate, FrameRate::new(24, 1));
        assert_eq!(comp.duration_frames, 120);
        assert_eq!(comp.background_color, Color::new(0.1, 0.2, 0.3, 1.0));
        assert_eq!(
            project.document().root_comp,
            Some(root),
            "creating a composition must not move the model root"
        );
        assert_eq!(
            project.playback_params(cx),
            Some((FrameRate::new(24, 1), 120)),
            "the transport follows the new composition"
        );
    });

    project.update(cx, |project, cx| project.undo(cx));
    project.read_with(cx, |project, _| {
        assert!(
            project.document().get_composition(created).is_none(),
            "one undo removes the whole composition"
        );
    });
}

/// Settings edits keep the layers and roll back in one step.
#[gpui::test]
fn editing_settings_keeps_the_layers_and_is_one_undo_step(cx: &mut TestAppContext) {
    let project = project(cx);
    let comp = project.read_with(cx, |project, _| project.document().root_comp.unwrap());
    let layer = LayerId::next();
    project.update(cx, |project, cx| {
        let doc = add_layer(
            project.document(),
            comp,
            Layer::new(layer, "Solid", Graph::new()).with_time(0, 0, 100),
        )
        .unwrap();
        project.commit_document(doc, InvalidationHint::Structural, cx);
    });

    project.update(cx, |project, cx| {
        project.apply_composition_settings(comp, settings("Renamed"), cx);
    });

    project.read_with(cx, |project, _| {
        let edited = project.document().get_composition(comp).unwrap();
        assert_eq!(edited.name, "Renamed");
        assert_eq!(edited.resolution, (1280, 720));
        assert_eq!(
            edited.layer_count(),
            1,
            "the layers survive a settings edit"
        );
    });

    project.update(cx, |project, cx| project.undo(cx));
    project.read_with(cx, |project, _| {
        let restored = project.document().get_composition(comp).unwrap();
        assert_eq!(restored.name, "Comp 1");
        assert_eq!(restored.layer_count(), 1);
    });
}

/// A duplicate is an independent copy, and the UI moves to it — the copy is
/// what the user goes on to edit.
#[gpui::test]
fn duplicating_a_composition_switches_to_the_independent_copy(cx: &mut TestAppContext) {
    let project = project(cx);
    let comp = project.read_with(cx, |project, _| project.document().root_comp.unwrap());
    let layer = LayerId::next();
    project.update(cx, |project, cx| {
        let doc = add_layer(
            project.document(),
            comp,
            Layer::new(layer, "Solid", Graph::new()).with_time(0, 0, 100),
        )
        .unwrap();
        project.commit_document(doc, InvalidationHint::Structural, cx);
    });

    let copy = project
        .update(cx, |project, cx| project.duplicate_composition(comp, cx))
        .expect("the copy exists");

    project.read_with(cx, |project, cx| {
        assert_eq!(panels::active_composition(cx), Some(copy));
        let copied = project.document().get_composition(copy).unwrap();
        assert_eq!(copied.name, "Comp 1 copy");
        assert_eq!(copied.layer_count(), 1);
        assert_ne!(
            copied.layers[0].id, layer,
            "a copied layer gets a fresh id, so edits cannot bleed across"
        );
        assert_eq!(
            project.document().get_composition(comp).unwrap().layers[0].id,
            layer,
            "the source composition is untouched"
        );
    });

    project.update(cx, |project, cx| project.undo(cx));
    project.read_with(cx, |project, _| {
        assert!(project.document().get_composition(copy).is_none());
    });
}

/// Deleting the active composition hands over to its neighbour; deleting the
/// last one is composition 0, which the app carries without panicking.
#[gpui::test]
fn deleting_a_composition_moves_the_active_one_to_its_neighbour(cx: &mut TestAppContext) {
    let project = project(cx);
    let first = project.read_with(cx, |project, _| project.document().root_comp.unwrap());
    let second = project.update(cx, |project, cx| {
        project.create_composition(settings("Second"), cx)
    });

    // The active composition is the second one; deleting it falls back to the
    // remaining neighbour.
    project.update(cx, |project, cx| project.delete_composition(second, cx));
    project.read_with(cx, |project, cx| {
        assert!(project.document().get_composition(second).is_none());
        assert_eq!(panels::active_composition(cx), Some(first));
        assert_eq!(panels::layer_selection(cx).comp(), Some(first));
    });

    // Deleting the last composition leaves a valid, composition-less state.
    project.update(cx, |project, cx| project.delete_composition(first, cx));
    project.read_with(cx, |project, cx| {
        assert!(project.document().compositions.is_empty());
        assert_eq!(project.document().root_comp, None);
        assert_eq!(panels::active_composition(cx), None);
        assert_eq!(project.playback_params(cx), None);
    });

    // …and a new composition brings the project back, adopting the model root.
    let created = project.update(cx, |project, cx| {
        project.create_composition(settings("Fresh"), cx)
    });
    project.read_with(cx, |project, cx| {
        assert_eq!(panels::active_composition(cx), Some(created));
        assert_eq!(
            project.document().root_comp,
            Some(created),
            "the first composition of an empty project becomes its root"
        );
    });
}

/// Deleting a composition that is *not* active leaves the active one alone.
#[gpui::test]
fn deleting_another_composition_keeps_the_active_one(cx: &mut TestAppContext) {
    let project = project(cx);
    let active = project.read_with(cx, |project, _| project.document().root_comp.unwrap());
    let other = project.update(cx, |project, cx| {
        let id = project.create_composition(settings("Other"), cx);
        // Switch back: `other` exists but is not what the UI shows.
        project.set_active_composition(Some(active), cx);
        id
    });

    project.update(cx, |project, cx| project.delete_composition(other, cx));

    project.read_with(cx, |project, cx| {
        assert!(project.document().get_composition(other).is_none());
        assert_eq!(panels::active_composition(cx), Some(active));
    });
}

/// The composition commands act on the composition the user pointed at: the
/// Outliner publishes it as the Properties target, and the commands read that
/// before falling back to the active composition.
#[gpui::test]
fn composition_commands_target_the_selected_row_then_the_active_composition(
    cx: &mut TestAppContext,
) {
    let project = project(cx);
    let active = project.read_with(cx, |project, _| project.document().root_comp.unwrap());
    let other = project.update(cx, |project, cx| {
        let id = project.create_composition(settings("Other"), cx);
        project.set_active_composition(Some(active), cx);
        id
    });

    cx.update(|cx| {
        assert_eq!(
            panels::command_target_composition(cx),
            Some(active),
            "with no composition selected the commands act on the active one"
        );
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Composition {
            comp_id: other,
        }));
        assert_eq!(
            panels::command_target_composition(cx),
            Some(other),
            "a selected composition row is what the commands act on"
        );
        // A layer target belongs to a layer, not to a composition row.
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Layer {
            comp_id: active,
            layer_id: LayerId::next(),
        }));
        assert_eq!(panels::command_target_composition(cx), Some(active));
    });
}

/// An unknown composition id is not an edit: no undo step, no panic.
#[gpui::test]
fn operations_on_a_missing_composition_do_nothing(cx: &mut TestAppContext) {
    let project = project(cx);
    let missing = CompId::next();
    let before = project.read_with(cx, |project, _| project.document().clone());

    project.update(cx, |project, cx| {
        project.apply_composition_settings(missing, settings("Nope"), cx);
        assert!(project.duplicate_composition(missing, cx).is_none());
        project.delete_composition(missing, cx);
    });

    project.read_with(cx, |project, _| {
        assert!(*project.document() == before);
    });
}

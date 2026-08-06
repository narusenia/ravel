// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Active composition and layer selection (REQ-UI-013, unit 1).
//!
//! `ProjectState` owns the [`panels::ActiveComposition`] global; every panel
//! resolves what it shows through it. These tests pin the invariants the rest
//! of the UI relies on: the selection always names the active composition,
//! switching never touches the document, and "no active composition" is a
//! state the app carries without panicking.
//!
//! They live in an integration test rather than a `mod tests` inside
//! `panels/mod.rs` because rustc overflows its stack while compiling this
//! crate's lib-test target with a test module in that file (reproduces with a
//! plain `#[test]`, with sccache and incremental compilation both disabled).

use gpui::{AppContext as _, TestAppContext};
use ravel_app::panels::{self, PropertiesTarget, SelectedPropertiesTarget};
use ravel_app::playback::PlaybackController;
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::{Composition, Layer};
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::FrameRate;
use ravel_ui::command::CommandId;

/// A project state with the default startup document (one root composition).
fn project(cx: &mut TestAppContext) -> gpui::Entity<ProjectState> {
    ravel_app::project_state::disable_background_eval_for_tests();
    cx.update(|cx| {
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        cx.set_global(SelectedPropertiesTarget::default());
        project
    })
}

/// Commits a second composition into the document and returns its id. The
/// document root is left alone — adding a composition never changes which one
/// the UI is on.
fn add_composition(
    project: &gpui::Entity<ProjectState>,
    cx: &mut TestAppContext,
    layer: LayerId,
) -> CompId {
    project.update(cx, |project, cx| {
        let id = CompId::next();
        let comp = Composition::new(id, "Other", (1280, 720), FrameRate::new(24, 1), 120)
            .add_layer(Layer::new(layer, "Solid", Graph::new()).with_time(0, 0, 120));
        let mut doc = project.document().clone();
        doc.compositions.insert(id, std::sync::Arc::new(comp));
        project.commit_document(doc, InvalidationHint::Structural, cx);
        id
    })
}

/// A document opens on its root composition, and the empty selection already
/// names it.
#[gpui::test]
fn startup_activates_the_document_root_composition(cx: &mut TestAppContext) {
    let project = project(cx);

    project.read_with(cx, |project, cx| {
        let root = project.document().root_comp;
        assert!(root.is_some());
        assert_eq!(panels::active_composition(cx), root);
        assert_eq!(
            project.active_composition(cx).map(|comp| comp.id),
            root,
            "the active id must resolve in the live document"
        );
        let selection = panels::layer_selection(cx);
        assert_eq!(selection.comp(), root);
        assert!(selection.is_empty());
    });
}

/// Switching compositions moves every derived consumer (playback params,
/// selection, Properties target) and leaves the document untouched — a switch
/// belongs in neither the undo history nor the saved file.
#[gpui::test]
fn switching_the_active_composition_leaves_the_document_alone(cx: &mut TestAppContext) {
    let project = project(cx);
    let layer = LayerId::next();
    let other = add_composition(&project, cx, layer);
    let root = project.read_with(cx, |project, _| project.document().root_comp.unwrap());

    // A selection in the root composition, with the Properties panel on it.
    cx.update(|cx| {
        panels::set_layer_selection(vec![layer], cx);
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Layer {
            comp_id: root,
            layer_id: layer,
        }));
    });

    let before = project.read_with(cx, |project, _| project.document().clone());
    project.update(cx, |project, cx| {
        assert_eq!(
            project.playback_params(cx),
            Some((FrameRate::new(30, 1), 300))
        );
        project.set_active_composition(Some(other), cx);
        assert_eq!(
            project.playback_params(cx),
            Some((FrameRate::new(24, 1), 120)),
            "the playback clock follows the active composition"
        );
        assert_eq!(
            project.document().root_comp,
            Some(root),
            "a UI switch must not rewrite the document root"
        );
        assert!(
            *project.document() == before,
            "a UI switch must not edit the document at all — no undo step, \
             no saved diff"
        );
    });

    cx.update(|cx| {
        assert_eq!(panels::active_composition(cx), Some(other));
        let selection = panels::layer_selection(cx);
        assert_eq!(
            selection.comp(),
            Some(other),
            "LayerSelection.comp == ActiveComposition"
        );
        assert!(
            selection.is_empty(),
            "a selection belongs to the composition it was made in"
        );
        assert!(matches!(
            cx.global::<SelectedPropertiesTarget>().0,
            PropertiesTarget::Empty
        ));
    });

    // The invariant survives selecting inside the new composition.
    cx.update(|cx| {
        panels::set_layer_selection(vec![layer], cx);
        let selection = panels::layer_selection(cx);
        assert_eq!(selection.comp(), panels::active_composition(cx));
        assert_eq!(panels::selected_layer(cx), Some(layer));
    });
}

/// Composition 0 is a legitimate state: consumers resolve to nothing and the
/// viewer blanks instead of panicking.
#[gpui::test]
fn no_active_composition_is_a_valid_state(cx: &mut TestAppContext) {
    let project = project(cx);
    let layer = LayerId::next();
    cx.update(|cx| panels::set_layer_selection(vec![layer], cx));

    project.update(cx, |project, cx| {
        project.set_active_composition(None, cx);
        assert!(project.active_composition(cx).is_none());
        assert_eq!(project.playback_params(cx), None);
        // Layer templates have nowhere to go instead of falling back to the
        // document root.
        assert_eq!(project.add_layer_from_template("solid", cx), None);
    });

    cx.update(|cx| {
        assert_eq!(panels::active_composition(cx), None);
        let selection = panels::layer_selection(cx);
        assert_eq!(selection.comp(), None);
        assert!(selection.is_empty());
        assert!(
            matches!(
                cx.global::<panels::ViewerFrame>(),
                panels::ViewerFrame::Blank {
                    composition_resolution: None
                }
            ),
            "the viewer blanks with no composition geometry"
        );
    });
}

/// The transport must not run over a composition that is not there: with no
/// active composition the clock adopts a zero-length range, so toggle and
/// frame-step are no-ops.
#[gpui::test]
fn playback_is_inert_without_an_active_composition(cx: &mut TestAppContext) {
    let project = project(cx);
    let controller = cx.update(|cx| cx.new(|_| PlaybackController::new()));

    // With a composition, a frame step advances as usual.
    cx.update(|cx| {
        controller.update(cx, |controller, cx| {
            controller.handle_command(CommandId::FrameStepForward, cx);
        });
    });
    cx.update(|cx| {
        assert_eq!(controller.read(cx).transport().current_frame(), 1);
    });

    project.update(cx, |project, cx| project.set_active_composition(None, cx));

    cx.update(|cx| {
        controller.update(cx, |controller, cx| {
            controller.handle_command(CommandId::FrameStepForward, cx);
            controller.handle_command(CommandId::PlaybackToggle, cx);
        });
    });
    cx.update(|cx| {
        let transport = controller.read(cx).transport();
        assert_eq!(transport.current_frame(), 0);
        assert!(!transport.is_playing(), "there is nothing to play");
    });
}

/// The active composition survives save → File ▸ New → File ▸ Open
/// (REQ-UI-013): it is persisted in `ui_state.json`, not in the document.
#[gpui::test]
fn the_active_composition_is_restored_by_a_save_and_load(cx: &mut TestAppContext) {
    let project = project(cx);
    let other = add_composition(&project, cx, LayerId::next());
    let root = project.read_with(cx, |project, _| project.document().root_comp.unwrap());

    let dir = std::env::temp_dir().join(format!("ravel_ui_state_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("active_comp.ravprj");
    let _ = std::fs::remove_file(&path);

    project.update(cx, |project, cx| {
        project.set_active_composition(Some(other), cx);
        project.save_project_to(path.clone(), None, cx);
    });
    cx.run_until_parked();

    // File ▸ New moves off the saved composition entirely.
    project.update(cx, |project, cx| project.new_document(cx));
    cx.update(|cx| {
        assert_ne!(panels::active_composition(cx), Some(other));
    });

    project.update(cx, |project, cx| {
        project.load_project_from(path.clone(), cx)
    });
    cx.run_until_parked();

    project.read_with(cx, |project, cx| {
        assert_eq!(
            panels::active_composition(cx),
            Some(other),
            "the saved session's composition, not the document root"
        );
        assert_eq!(project.document().root_comp, Some(root));
        // The invariant holds across the restore.
        assert_eq!(panels::layer_selection(cx).comp(), Some(other));
        assert!(panels::layer_selection(cx).is_empty());
    });

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
    let _ = std::fs::remove_dir(&dir);
}

/// A layer the document has lost leaves the selection even when no panel is
/// alive to notice (REQ-UI-013): the `motion` and `node` workspaces have no
/// Timeline, so `ProjectState` owns this — including through undo.
#[gpui::test]
fn a_deleted_layer_leaves_the_selection_without_any_panel(cx: &mut TestAppContext) {
    let project = project(cx);
    let (comp, kept, removed) = project.update(cx, |project, cx| {
        let comp = project.document().root_comp.expect("root comp");
        let (kept, removed) = (LayerId::next(), LayerId::next());
        let mut doc = project.document().clone();
        for (id, name) in [(kept, "Kept"), (removed, "Removed")] {
            doc = ravel_ui::document::add_layer(
                &doc,
                comp,
                Layer::new(id, name, Graph::new()).with_time(0, 0, 100),
            )
            .unwrap();
        }
        project.commit_document(doc, InvalidationHint::Structural, cx);
        (comp, kept, removed)
    });

    cx.update(|cx| {
        panels::set_layer_selection(vec![kept, removed], cx);
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Layers {
            comp_id: comp,
            layer_ids: vec![kept, removed],
        }));
    });

    project.update(cx, |project, cx| {
        let doc = ravel_ui::document::remove_layer(project.document(), comp, removed).unwrap();
        project.commit_document(doc, InvalidationHint::Structural, cx);
    });

    cx.update(|cx| {
        assert_eq!(
            panels::layer_selection(cx).layers(),
            [kept],
            "the deleted layer left, the rest of the selection stayed"
        );
        assert!(
            matches!(
                &cx.global::<SelectedPropertiesTarget>().0,
                PropertiesTarget::Layer { comp_id, layer_id } if *comp_id == comp && *layer_id == kept
            ),
            "Properties follows the shrunken selection instead of emptying"
        );
    });

    // Undo brings the layer back but not the selection — a selection is not
    // part of the document snapshot.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    cx.update(|cx| {
        assert_eq!(panels::layer_selection(cx).layers(), [kept]);
    });
}

/// The active composition can vanish from the document (an undo past its
/// creation). Which composition is active stays out of the undo history by
/// design, but nothing can stay selected inside a composition that is gone.
#[gpui::test]
fn losing_the_active_composition_empties_the_selection(cx: &mut TestAppContext) {
    let project = project(cx);
    let layer = LayerId::next();
    let other = add_composition(&project, cx, layer);

    project.update(cx, |project, cx| {
        project.set_active_composition(Some(other), cx)
    });
    cx.update(|cx| {
        panels::set_layer_selection(vec![layer], cx);
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Layer {
            comp_id: other,
            layer_id: layer,
        }));
    });

    // The composition disappears without anyone handing the active id over —
    // what an undo past `create_composition` does.
    project.update(cx, |project, cx| {
        let doc = ravel_ui::document::remove_composition(project.document(), other).unwrap();
        project.commit_document(doc, InvalidationHint::Structural, cx);
    });

    cx.update(|cx| {
        assert_eq!(
            panels::active_composition(cx),
            Some(other),
            "a composition switch is not part of the undo history"
        );
        let selection = panels::layer_selection(cx);
        assert!(
            selection.is_empty(),
            "no layer can be selected inside a composition the document lost"
        );
        assert_eq!(selection.comp(), Some(other), "the invariant still holds");
        assert!(matches!(
            cx.global::<SelectedPropertiesTarget>().0,
            PropertiesTarget::Empty
        ));
    });
}

/// File ▸ New opens the fresh document on its own root composition and drops
/// the previous selection.
#[gpui::test]
fn a_replaced_document_activates_its_own_root(cx: &mut TestAppContext) {
    let project = project(cx);
    let (old_root, layer) = (
        project.read_with(cx, |project, _| project.document().root_comp.unwrap()),
        LayerId::next(),
    );
    cx.update(|cx| panels::set_layer_selection(vec![layer], cx));

    project.update(cx, |project, cx| project.new_document(cx));

    project.read_with(cx, |project, cx| {
        let root = project.document().root_comp.expect("a fresh root comp");
        assert_ne!(root, old_root);
        assert_eq!(panels::active_composition(cx), Some(root));
        let selection = panels::layer_selection(cx);
        assert_eq!(selection.comp(), Some(root));
        assert!(selection.is_empty());
    });
}

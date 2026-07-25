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
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::{Composition, Layer};
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId};
use ravel_core::runtime::InvalidationHint;
use ravel_core::types::FrameRate;

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

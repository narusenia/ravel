// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The panel rebuild gate (RESP-2 of
//! `docs/implementation/ui-responsiveness-plan.md`, issue HIGH-07).
//!
//! Every panel that mirrors the document observes `ProjectState`, and the
//! callback is the expensive half — a `Composition` or `Graph` deep compare, a
//! full row walk, a section rebuild. `ProjectState` also notifies for state no
//! panel mirrors, so each panel holds the epoch it last synced and returns
//! early when it has not moved. This test drives a real notify of each kind
//! through a live panel: the gate has to absorb one and pass the other.

use gpui::{
    AppContext as _, Entity, ParentElement as _, Pixels, Size, Styled as _, TestAppContext,
    WindowHandle, px,
};
use gpui_component::Root;
use ravel_app::panels::{self, node_editor::NodeEditorPanel};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::runtime::InvalidationHint;

const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(800.0),
    height: px(600.0),
};

struct TestRoot {
    panel: Entity<NodeEditorPanel>,
}

impl gpui::Render for TestRoot {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div().size_full().child(self.panel.clone())
    }
}

/// Counts notifications from whatever entity it was built to observe.
struct Probe {
    count: usize,
    _sub: gpui::Subscription,
}

struct Harness {
    _window: WindowHandle<Root>,
    project: Entity<ProjectState>,
    /// Notifications reaching `ProjectState` observers.
    project_probe: Entity<Probe>,
    /// Rebuild-and-repaint requests the Node Editor made in response.
    panel_probe: Entity<Probe>,
}

fn open_node_editor(cx: &mut TestAppContext) -> Harness {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
    ravel_app::project_state::disable_background_eval_for_tests();

    let project = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(panels::FocusedPanelGlobal(None));
        cx.set_global(panels::SelectedPropertiesTarget::default());
        cx.set_global(panels::CanvasSelection::default());
        cx.set_global(panels::LayerSelection::default());
        cx.set_global(panels::PlaybackPosition::default());
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        project
    });

    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured_in_window = captured.clone();
    let window = cx.open_window(WINDOW_SIZE, move |window, cx| {
        let panel = cx.new(|cx| NodeEditorPanel::new(window, cx));
        *captured_in_window.borrow_mut() = Some(panel.clone());
        Root::new(cx.new(|_| TestRoot { panel }), window, cx)
    });
    let panel: Entity<NodeEditorPanel> = captured
        .borrow_mut()
        .take()
        .expect("panel entity should be created");

    let observed = project.clone();
    let project_probe = cx.new(|cx| Probe {
        count: 0,
        _sub: cx.observe(&observed, |this: &mut Probe, _project, _cx| {
            this.count += 1;
        }),
    });
    let panel_probe = cx.new(|cx| Probe {
        count: 0,
        _sub: cx.observe(&panel, |this: &mut Probe, _panel, _cx| {
            this.count += 1;
        }),
    });
    cx.run_until_parked();
    Harness {
        _window: window,
        project,
        project_probe,
        panel_probe,
    }
}

/// `(ProjectState observers, Node Editor)` notification counts.
fn counts(harness: &Harness, cx: &mut TestAppContext) -> (usize, usize) {
    (
        harness.project_probe.read_with(cx, |probe, _| probe.count),
        harness.panel_probe.read_with(cx, |probe, _| probe.count),
    )
}

/// Add an empty-network layer to the root composition and return its id.
fn add_layer(harness: &Harness, cx: &mut TestAppContext) -> ravel_core::id::LayerId {
    let layer = ravel_core::id::LayerId::next();
    harness.project.update(cx, |project, cx| {
        let comp = project.document().root_comp.expect("root comp");
        let document = ravel_ui::document::add_layer(
            project.document(),
            comp,
            ravel_core::composition::Layer::new(layer, "Solid 1", ravel_core::graph::Graph::new()),
        )
        .unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    cx.run_until_parked();
    layer
}

#[gpui::test]
fn a_completed_save_notifies_the_project_but_rebuilds_no_panel(cx: &mut TestAppContext) {
    let harness = open_node_editor(cx);

    let dir = std::env::temp_dir().join(format!("ravel_gate_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gate.ravprj");
    let _ = std::fs::remove_file(&path);

    // A document edit is what the panel exists to follow: it must get through.
    let (project_before, panel_before) = counts(&harness, cx);
    add_layer(&harness, cx);
    let (project_after_edit, panel_after_edit) = counts(&harness, cx);
    assert!(
        project_after_edit > project_before,
        "the edit must notify project observers"
    );
    assert!(
        panel_after_edit > panel_before,
        "the panel must rebuild for a document edit"
    );

    // A completed save reaches the same observers — the window title follows
    // the project path — but changes nothing the panel mirrors.
    harness
        .project
        .update(cx, |project, cx| project.save_project_to(path.clone(), cx));
    cx.run_until_parked();
    let (project_after_save, panel_after_save) = counts(&harness, cx);
    assert!(
        !harness
            .project
            .read_with(cx, |project, _| project.is_dirty()),
        "the save must have completed"
    );
    assert!(
        project_after_save > project_after_edit,
        "the completed save must notify project observers (window title)"
    );
    assert_eq!(
        panel_after_save, panel_after_edit,
        "the gate must absorb a notify that left the document alone"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(ravel_app::project::container::backup_path(&path));
    let _ = std::fs::remove_dir(&dir);
}

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
    /// Stands in for the second wave: every `CanvasSelection` publication
    /// wakes the Viewer's gesture-target walk and the Outliner's repaint,
    /// whether or not the value changed.
    selection_probe: Entity<Probe>,
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
    // Counts publications, not value changes: `set_global` wakes observers
    // even when it writes an identical value, which is the whole cost HIGH-07
    // is about.
    let selection_probe = cx.new(|cx| Probe {
        count: 0,
        _sub: cx.observe_global::<panels::CanvasSelection>(|this: &mut Probe, _cx| {
            this.count += 1;
        }),
    });
    cx.run_until_parked();
    Harness {
        _window: window,
        project,
        project_probe,
        panel_probe,
        selection_probe,
    }
}

/// `(ProjectState observers, Node Editor, CanvasSelection publications)`.
fn counts(harness: &Harness, cx: &mut TestAppContext) -> (usize, usize, usize) {
    (
        harness.project_probe.read_with(cx, |probe, _| probe.count),
        harness.panel_probe.read_with(cx, |probe, _| probe.count),
        harness
            .selection_probe
            .read_with(cx, |probe, _| probe.count),
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
    let (project_before, panel_before, _) = counts(&harness, cx);
    add_layer(&harness, cx);
    let (project_after_edit, panel_after_edit, _) = counts(&harness, cx);
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
    let (project_after_save, panel_after_save, _) = counts(&harness, cx);
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

/// The second wave HIGH-07 describes: the Node Editor used to republish
/// `CanvasSelection` on every graph change, so each mouse move of a parameter
/// drag woke the global's observers — the Viewer walking the document to
/// validate its gesture targets, the Outliner repainting — even though the
/// selection had not moved. Republishing an identical selection must be a
/// no-op now.
#[gpui::test]
fn an_unchanged_selection_is_not_republished(cx: &mut TestAppContext) {
    let harness = open_node_editor(cx);
    let layer = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    let path = ravel_ui::document::NetworkPath::layer(comp, layer);

    // Open the layer's network so the editor holds a context and a published
    // selection path.
    cx.update(|cx| panels::set_layer_selection(vec![layer], cx));
    cx.run_until_parked();
    let opened = cx.update(|cx| {
        cx.try_global::<panels::CanvasSelection>()
            .cloned()
            .unwrap_or_default()
    });
    assert_eq!(
        opened.path.as_ref(),
        Some(&path),
        "the editor should have opened the layer network"
    );

    // Now change the graph the editor is showing — the same thing a parameter
    // drag does on every mouse move. The selection is unaffected by it.
    let (_, panel_before, published_before) = counts(&harness, cx);
    harness.project.update(cx, |project, cx| {
        let network = ravel_core::graph::Graph::new()
            .add_node(
                ravel_core::graph::Node::new(ravel_core::id::NodeId::next(), "constant")
                    .with_param("value", ravel_core::graph::ParameterValue::Float(1.0)),
            )
            .unwrap();
        let document =
            ravel_ui::document::replace_network(project.document(), &path, network).unwrap();
        project.apply_document(document, InvalidationHint::Structural, cx);
    });
    cx.run_until_parked();
    let (_, panel_after, published_after) = counts(&harness, cx);

    assert!(
        panel_after > panel_before,
        "the editor itself must still follow the document"
    );
    assert_eq!(
        published_after, published_before,
        "an unchanged selection must not be republished"
    );
    // The selection is still the one the network opened with.
    let after = cx.update(|cx| {
        cx.try_global::<panels::CanvasSelection>()
            .cloned()
            .unwrap_or_default()
    });
    assert_eq!(after, opened, "the selection itself must be preserved");
}

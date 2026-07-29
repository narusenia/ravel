// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fan-out of an evaluation result to the panels (RESP-1 of
//! `docs/implementation/done/ui-responsiveness-plan.md`, issue CRIT-01).
//!
//! `ProjectState::on_eval_update` publishes globals and deliberately does not
//! notify its own observers, so the five panels that mirror the document stop
//! rebuilding once per evaluated frame. The one panel that draws evaluation
//! output outside the Viewer — the Node Editor's per-node load readout —
//! therefore has to follow the timings global itself. This test covers that
//! wiring, which needs a real window; the "no observer is notified" half is a
//! headless unit test in `project_state.rs`.

use gpui::{
    AppContext as _, Entity, ParentElement as _, Pixels, Size, Styled as _, TestAppContext,
    WindowHandle, px,
};
use gpui_component::Root;
use ravel_app::panels::{self, node_editor::NodeEditorPanel};
use ravel_app::project_state::{NodeEvalTimings, ProjectState, ProjectStateHandle};
use ravel_core::{
    composition::Layer,
    graph::Graph,
    id::{LayerId, NodeId},
    registry::{NodeRegistry, builtin::register_builtins},
    runtime::InvalidationHint,
};
use ravel_ui::document::NetworkPath;
use std::time::Duration;

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

/// Counts how often the observed panel asked to repaint.
struct RepaintProbe {
    repaints: usize,
    _sub: gpui::Subscription,
}

struct Harness {
    _window: WindowHandle<Root>,
    panel: Entity<NodeEditorPanel>,
    probe: Entity<RepaintProbe>,
    displayed_node: NodeId,
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

    let displayed_node = NodeId::next();
    let path = project.update(cx, |project, cx| {
        let comp = project.document().root_comp.expect("root composition");
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let node = registry
            .create_node("blur", displayed_node)
            .expect("blur node");
        let network = Graph::new().add_node(node).expect("valid network");
        let layer = Layer::new(LayerId::next(), "Timing Test", network).with_time(0, 0, 300);
        let path = NetworkPath::layer(comp, layer.id);
        let document = ravel_ui::document::add_layer(project.document(), comp, layer)
            .expect("layer can be added");
        project.commit_document(document, InvalidationHint::Structural, cx);
        path
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
    panel.update(cx, |panel, cx| panel.open_network(path, cx));

    let probe = cx.new(|cx| RepaintProbe {
        repaints: 0,
        _sub: cx.observe(&panel, |this: &mut RepaintProbe, _panel, _cx| {
            this.repaints += 1;
        }),
    });
    cx.run_until_parked();
    Harness {
        _window: window,
        panel,
        probe,
        displayed_node,
    }
}

#[gpui::test]
fn timings_publication_repaints_the_node_editor(cx: &mut TestAppContext) {
    let harness = open_node_editor(cx);
    let baseline = harness.probe.read_with(cx, |probe, _| probe.repaints);

    // A global the panel does not follow must not wake it: without this the
    // assertion below could pass on unrelated churn.
    cx.update(|cx| panels::set_media_selection(vec!["asset".to_string()], cx));
    cx.run_until_parked();
    assert_eq!(
        harness.probe.read_with(cx, |probe, _| probe.repaints),
        baseline,
        "the Node Editor must not repaint for an unrelated global"
    );

    // The load readout follows the timings global directly, so publishing one
    // evaluation's durations repaints the panel without any document change.
    cx.update(|cx| {
        let mut timings = NodeEvalTimings::default();
        timings
            .0
            .insert(harness.displayed_node, Duration::from_micros(750));
        cx.set_global(timings);
    });
    cx.run_until_parked();
    assert!(
        harness.probe.read_with(cx, |probe, _| probe.repaints) > baseline,
        "publishing per-node timings must repaint the Node Editor load readout"
    );

    let displayed_baseline = harness.probe.read_with(cx, |probe, _| probe.repaints);
    cx.update(|cx| {
        let mut timings = NodeEvalTimings::default();
        timings
            .0
            .insert(harness.displayed_node, Duration::from_micros(750));
        timings.0.insert(NodeId::next(), Duration::from_micros(900));
        cx.set_global(timings);
    });
    cx.run_until_parked();
    assert_eq!(
        harness.probe.read_with(cx, |probe, _| probe.repaints),
        displayed_baseline,
        "timings outside the displayed network must not repaint the Node Editor"
    );

    harness
        .panel
        .update(cx, |panel, cx| panel.close_network(cx));
    cx.run_until_parked();
    let closed_baseline = harness.probe.read_with(cx, |probe, _| probe.repaints);
    cx.update(|cx| {
        let mut timings = NodeEvalTimings::default();
        timings
            .0
            .insert(harness.displayed_node, Duration::from_micros(1_200));
        cx.set_global(timings);
    });
    cx.run_until_parked();
    assert_eq!(
        harness.probe.read_with(cx, |probe, _| probe.repaints),
        closed_baseline,
        "timings must not repaint the Node Editor while no network is open"
    );
}

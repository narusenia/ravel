// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The panel rebuild gate (RESP-2 of
//! `docs/implementation/done/ui-responsiveness-plan.md`, issue HIGH-07) and the
//! per-gesture sync cost measured through `panels::sync_probe`.
//!
//! Every panel that mirrors the document observes `ProjectState`, and the
//! callback is the expensive half — a `Composition` or `Graph` deep compare, a
//! full row walk, a section rebuild. `ProjectState` also notifies for state no
//! panel mirrors, so each panel holds the epoch it last synced and returns
//! early when it has not moved. This test drives a real notify of each kind
//! through a live panel: the gate has to absorb one and pass the other.
//!
//! Notification probes cannot answer the second question — GPUI coalesces the
//! `cx.notify()` calls of one effect cycle, so one callback may stand for any
//! number of sync-function entries. The `sync_probe` counters do: the
//! `*_sync_counts` tests below drive one gesture and assert how many times each
//! sync function actually ran. `docs/implementation/perf-baseline.md` records
//! how to run them and what the numbers were.

use gpui::{
    AppContext as _, Entity, ParentElement as _, Pixels, Size, Styled as _, TestAppContext,
    WindowHandle, px,
};
use gpui_component::Root;
use ravel_app::panels::{
    self, media_bin::MediaBinGpuiPanel, node_editor::NodeEditorPanel, outliner::OutlinerGpuiPanel,
    properties::PropertiesGpuiPanel, timeline::TimelineGpuiPanel,
};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::runtime::InvalidationHint;

const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(800.0),
    height: px(600.0),
};

/// All five panels that mirror the document, so a gate removed from any one of
/// them fails a test.
struct Panels {
    node_editor: Entity<NodeEditorPanel>,
    timeline: Entity<TimelineGpuiPanel>,
    outliner: Entity<OutlinerGpuiPanel>,
    media_bin: Entity<MediaBinGpuiPanel>,
    properties: Entity<PropertiesGpuiPanel>,
}

struct TestRoot {
    panels: Panels,
}

impl gpui::Render for TestRoot {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
            .size_full()
            .child(self.panels.node_editor.clone())
            .child(self.panels.timeline.clone())
            .child(self.panels.outliner.clone())
            .child(self.panels.media_bin.clone())
            .child(self.panels.properties.clone())
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
    /// One probe per document-mirroring panel, `(name, probe)`.
    panel_probes: Vec<(&'static str, Entity<Probe>)>,
    /// Stands in for the second wave: every `CanvasSelection` publication
    /// wakes the Viewer's gesture-target walk and the Outliner's repaint,
    /// whether or not the value changed.
    selection_probe: Entity<Probe>,
}

fn open_panels(cx: &mut TestAppContext) -> Harness {
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
        let panels = Panels {
            node_editor: cx
                .new(|cx| NodeEditorPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)),
            timeline: cx
                .new(|cx| TimelineGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)),
            outliner: cx
                .new(|cx| OutlinerGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)),
            media_bin: cx
                .new(|cx| MediaBinGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)),
            properties: cx.new(|cx| {
                PropertiesGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
            }),
        };
        *captured_in_window.borrow_mut() = Some((
            panels.node_editor.clone(),
            panels.timeline.clone(),
            panels.outliner.clone(),
            panels.media_bin.clone(),
            panels.properties.clone(),
        ));
        Root::new(cx.new(|_| TestRoot { panels }), window, cx)
    });
    let (node_editor, timeline, outliner, media_bin, properties) = captured
        .borrow_mut()
        .take()
        .expect("panel entities should be created");

    let observed = project.clone();
    let project_probe = cx.new(|cx| Probe {
        count: 0,
        _sub: cx.observe(&observed, |this: &mut Probe, _project, _cx| {
            this.count += 1;
        }),
    });
    let panel_probes = vec![
        ("node_editor", probe_entity(&node_editor, cx)),
        ("timeline", probe_entity(&timeline, cx)),
        ("outliner", probe_entity(&outliner, cx)),
        ("media_bin", probe_entity(&media_bin, cx)),
        ("properties", probe_entity(&properties, cx)),
    ];
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
        panel_probes,
        selection_probe,
    }
}

/// A probe counting notifications from `entity`.
fn probe_entity<T: 'static>(entity: &Entity<T>, cx: &mut TestAppContext) -> Entity<Probe> {
    let entity = entity.clone();
    cx.new(|cx| Probe {
        count: 0,
        _sub: cx.observe(&entity, |this: &mut Probe, _entity, _cx| {
            this.count += 1;
        }),
    })
}

/// Per-panel notification counts, in `panel_probes` order.
fn panel_counts(harness: &Harness, cx: &mut TestAppContext) -> Vec<(&'static str, usize)> {
    harness
        .panel_probes
        .iter()
        .map(|(name, probe)| (*name, probe.read_with(cx, |probe, _| probe.count)))
        .collect()
}

fn project_count(harness: &Harness, cx: &mut TestAppContext) -> usize {
    harness.project_probe.read_with(cx, |probe, _| probe.count)
}

fn selection_count(harness: &Harness, cx: &mut TestAppContext) -> usize {
    harness
        .selection_probe
        .read_with(cx, |probe, _| probe.count)
}

fn panel_count(harness: &Harness, name: &str, cx: &mut TestAppContext) -> usize {
    harness
        .panel_probes
        .iter()
        .find(|(probe_name, _)| *probe_name == name)
        .map(|(_, probe)| probe.read_with(cx, |probe, _| probe.count))
        .unwrap_or_else(|| panic!("no probe named {name}"))
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

// ---------------------------------------------------------------------------
// Per-gesture sync cost
// ---------------------------------------------------------------------------

/// Executions of each mirrored sync function since the last [`reset_syncs`],
/// as `(name, count)` — printed by every scenario below so the measurement
/// procedure is `cargo test` plus `--nocapture`.
#[cfg(debug_assertions)]
fn sync_counts(scenario: &str) -> Vec<(&'static str, u64)> {
    use panels::sync_probe::{PanelSync, count};
    let counts = vec![
        (
            "properties.refresh_values",
            count(PanelSync::PropertiesRefresh),
        ),
        ("timeline.sync_from_project", count(PanelSync::TimelineSync)),
        ("outliner.rebuild_rows", count(PanelSync::OutlinerRows)),
        ("media_bin.rebuild_rows", count(PanelSync::MediaBinRows)),
    ];
    println!("sync counts [{scenario}]:");
    for (name, value) in &counts {
        println!("  {name}: {value}");
    }
    counts
}

#[cfg(debug_assertions)]
fn reset_syncs() {
    panels::sync_probe::reset();
}

#[cfg(debug_assertions)]
fn count_of(counts: &[(&'static str, u64)], name: &str) -> u64 {
    counts
        .iter()
        .find(|(probe, _)| *probe == name)
        .map(|(_, value)| *value)
        .unwrap_or_else(|| panic!("no counter named {name}"))
}

/// Open `layer`'s network with one `constant` node and select the layer, the
/// state a node parameter drag runs against. Returns the network path and the
/// node the drag moves.
#[cfg(debug_assertions)]
fn open_layer_network(
    harness: &Harness,
    layer: ravel_core::id::LayerId,
    cx: &mut TestAppContext,
) -> (ravel_ui::document::NetworkPath, ravel_core::id::NodeId) {
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    let path = ravel_ui::document::NetworkPath::layer(comp, layer);
    let node = ravel_core::id::NodeId::next();
    harness.project.update(cx, |project, cx| {
        let network = ravel_core::graph::Graph::new()
            .add_node(
                ravel_core::graph::Node::new(node, "constant")
                    .with_param("value", ravel_core::graph::ParameterValue::Float(0.0)),
            )
            .unwrap();
        let document =
            ravel_ui::document::replace_network(project.document(), &path, network).unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    cx.update(|cx| panels::set_layer_selection(vec![layer], cx));
    cx.run_until_parked();
    // Select the node the way a click in the editor would: the editor
    // republishes the Properties target from `refresh_from_document` only while
    // its selection is non-empty, and that republish is the second of the two
    // paths MED-UI-06 describes.
    cx.update(|cx| {
        cx.set_global(panels::CanvasSelection {
            path: Some(path.clone()),
            nodes: std::iter::once(node).collect(),
        });
    });
    cx.run_until_parked();
    (path, node)
}

/// One mouse move of a node parameter drag: a new value on an existing node,
/// applied live with the `Params` hint (`apply_document`, no undo step) — the
/// shape `NodeEditorPanel` and the Viewer gizmos both use.
#[cfg(debug_assertions)]
fn drag_tick(
    harness: &Harness,
    path: &ravel_ui::document::NetworkPath,
    node: ravel_core::id::NodeId,
    value: f32,
    cx: &mut TestAppContext,
) {
    harness.project.update(cx, |project, cx| {
        let network = ravel_ui::document::resolve_network(project.document(), path)
            .expect("the layer network is open")
            .clone()
            .set_params(
                node,
                &[ravel_core::graph::Parameter {
                    key: "value".into(),
                    value: ravel_core::graph::ParameterValue::Float(value),
                }],
            )
            .expect("the constant node has a value parameter");
        let document =
            ravel_ui::document::replace_network(project.document(), path, network).unwrap();
        project.apply_document(document, InvalidationHint::Params(vec![node]), cx);
    });
    cx.run_until_parked();
}

/// Ten mouse moves of a node parameter drag, with the dragged node selected —
/// the gesture MED-UI-01/04/05/06 are all about. A drag changes the document on
/// every move, so every mirror is entitled to one sync per move and to no more
/// than one.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_parameter_drag_sync_counts(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let layer = add_layer(&harness, cx);
    let (path, node) = open_layer_network(&harness, layer, cx);

    const MOVES: u64 = 10;
    reset_syncs();
    for step in 0..MOVES {
        drag_tick(&harness, &path, node, step as f32, cx);
    }
    let counts = sync_counts("node parameter drag, 10 moves");

    for (name, value) in &counts {
        assert!(
            *value >= MOVES,
            "{name} must follow every move of the drag ({value} for {MOVES} moves)"
        );
    }
    for name in [
        "timeline.sync_from_project",
        "outliner.rebuild_rows",
        "media_bin.rebuild_rows",
    ] {
        assert_eq!(
            count_of(&counts, name),
            MOVES,
            "{name} must not sync more than once per move"
        );
    }
    // MED-UI-06, still open: the node editor republishes the Properties target
    // from `refresh_from_document` on every move, and the `ProjectState` notify
    // of the same move resolves the sections a second time. Measured baseline.
    assert!(
        count_of(&counts, "properties.refresh_values") > MOVES,
        "the doubled re-resolve MED-UI-06 describes should still be visible here"
    );
}

/// One second of playback at 30 fps. Nothing about the document changes, so the
/// only panel entitled to sync is Properties, whose sections sample animated
/// channels at the playhead.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_second_of_playback_sync_counts(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let layer = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    cx.update(|cx| {
        cx.set_global(panels::SelectedPropertiesTarget(
            panels::PropertiesTarget::Layer {
                comp_id: comp,
                layer_id: layer,
            },
        ));
    });
    cx.run_until_parked();

    const FRAMES: u64 = 30;
    reset_syncs();
    for frame in 0..FRAMES {
        cx.update(|cx| {
            cx.set_global(panels::PlaybackPosition {
                frame,
                fps: ravel_core::types::FrameRate::new(30, 1),
            });
        });
        cx.run_until_parked();
    }
    let counts = sync_counts("playback, 30 frames");

    assert_eq!(
        count_of(&counts, "properties.refresh_values"),
        FRAMES,
        "Properties samples the playhead once per frame, not twice"
    );
    for name in [
        "timeline.sync_from_project",
        "outliner.rebuild_rows",
        "media_bin.rebuild_rows",
    ] {
        assert_eq!(
            count_of(&counts, name),
            0,
            "{name} mirrors the document, which playback does not change"
        );
    }
}

/// One composition switch. Timeline and Outliner sync from the
/// `ActiveComposition` global, and `set_active_composition` also notifies
/// `ProjectState` — the pair MED-UI-06 describes.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_composition_switch_sync_counts(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    add_layer(&harness, cx);
    let root = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    harness.project.update(cx, |project, cx| {
        project.create_composition(
            ravel_ui::document::CompositionSettings::fallback("Other"),
            cx,
        )
    });
    cx.run_until_parked();

    reset_syncs();
    harness.project.update(cx, |project, cx| {
        project.set_active_composition(Some(root), cx)
    });
    cx.run_until_parked();
    let counts = sync_counts("composition switch");

    assert_eq!(
        cx.update(|cx| panels::active_composition(cx)),
        Some(root),
        "the switch must have taken effect"
    );
    // MED-UI-06, still open: each panel syncs once from the `ActiveComposition`
    // observer and once more from the `ProjectState` notify of the same switch.
    // Measured baseline.
    for name in ["timeline.sync_from_project", "outliner.rebuild_rows"] {
        assert_eq!(
            count_of(&counts, name),
            2,
            "{name} should still sync twice for one composition switch"
        );
    }
}

#[gpui::test]
fn a_completed_save_rebuilds_no_document_panel(cx: &mut TestAppContext) {
    let harness = open_panels(cx);

    let dir = std::env::temp_dir().join(format!("ravel_gate_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gate.ravprj");
    let _ = std::fs::remove_file(&path);

    // Properties returns early with no target, so give it one: it resolves
    // values for the selected layer from the live document, which is the work
    // the gate is there to skip.
    let layer = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    cx.update(|cx| {
        cx.set_global(panels::SelectedPropertiesTarget(
            panels::PropertiesTarget::Layer {
                comp_id: comp,
                layer_id: layer,
            },
        ));
    });
    cx.run_until_parked();

    // A document edit is what these panels exist to follow: it must get
    // through to every one of them. (This also primes each gate, which starts
    // unset so a panel whose constructor does not sync cannot start out stale.)
    let project_before = project_count(&harness, cx);
    let before = panel_counts(&harness, cx);
    add_layer(&harness, cx);
    let after_edit = panel_counts(&harness, cx);
    assert!(
        project_count(&harness, cx) > project_before,
        "the edit must notify project observers"
    );
    for ((name, before), (_, after)) in before.iter().zip(after_edit.iter()) {
        assert!(
            after > before,
            "{name} must rebuild for a document edit ({before} -> {after})"
        );
    }

    // A completed save reaches the same observers — the window title follows
    // the project path — but changes nothing any of them mirrors.
    let project_after_edit = project_count(&harness, cx);
    harness.project.update(cx, |project, cx| {
        project.save_project_to(path.clone(), None, cx)
    });
    cx.run_until_parked();
    assert!(
        !harness
            .project
            .read_with(cx, |project, _| project.is_dirty()),
        "the save must have completed"
    );
    assert!(
        project_count(&harness, cx) > project_after_edit,
        "the completed save must notify project observers (window title)"
    );
    for ((name, expected), (_, actual)) in after_edit.iter().zip(panel_counts(&harness, cx).iter())
    {
        assert_eq!(
            actual, expected,
            "{name} must not rebuild for a notify that left the document alone"
        );
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
    let _ = std::fs::remove_dir(&dir);
}

/// A composition switch is the global-driven path: Timeline and Outliner sync
/// from `ActiveComposition`, and each records the epoch so the `ProjectState`
/// notify of the same switch is absorbed rather than repeating the walk.
///
/// The saving itself is not directly observable — GPUI coalesces the two
/// `cx.notify()` calls of one effect cycle into a single observer callback, so
/// a probe cannot count sync-function entries. What this test does cover is the
/// hazard of recording an epoch outside the gate's own observer: a panel must
/// not end up with a gate that swallows the *next* real change.
#[gpui::test]
fn a_composition_switch_leaves_every_gate_open_for_the_next_edit(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    add_layer(&harness, cx);
    let root = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp);
    let other = harness.project.update(cx, |project, cx| {
        project.create_composition(
            ravel_ui::document::CompositionSettings::fallback("Other"),
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| panels::active_composition(cx)),
        Some(other),
        "creating a composition opens it"
    );

    harness
        .project
        .update(cx, |project, cx| project.set_active_composition(root, cx));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| panels::active_composition(cx)),
        root,
        "the switch must have taken effect"
    );

    // The edit after the switch must still reach every panel.
    let before = panel_counts(&harness, cx);
    add_layer(&harness, cx);
    for ((name, before), (_, after)) in before.iter().zip(panel_counts(&harness, cx).iter()) {
        // Properties has no target here, so it legitimately stays put.
        if *name == "properties" {
            continue;
        }
        assert!(
            after > before,
            "{name} must still follow the document after a composition switch \
             ({before} -> {after})"
        );
    }
}

/// The second wave HIGH-07 describes: the Node Editor used to republish
/// `CanvasSelection` on every graph change, so each mouse move of a parameter
/// drag woke the global's observers — the Viewer walking the document to
/// validate its gesture targets, the Outliner repainting — even though the
/// selection had not moved. Republishing an identical selection must be a
/// no-op now.
#[gpui::test]
fn an_unchanged_selection_is_not_republished(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
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
    let editor_before = panel_count(&harness, "node_editor", cx);
    let published_before = selection_count(&harness, cx);
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

    assert!(
        panel_count(&harness, "node_editor", cx) > editor_before,
        "the editor itself must still follow the document"
    );
    assert_eq!(
        selection_count(&harness, cx),
        published_before,
        "an unchanged selection must not be republished"
    );
    let after = cx.update(|cx| {
        cx.try_global::<panels::CanvasSelection>()
            .cloned()
            .unwrap_or_default()
    });
    assert_eq!(after, opened, "the selection itself must be preserved");
}

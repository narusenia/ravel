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
//! early when it has not moved. MediaBin adds a narrower media-assets check:
//! layer-only document edits do not change the rows it displays. This test
//! drives a real notify of each kind through live panels: each gate has to
//! absorb irrelevant work and pass relevant work.
//!
//! Notification probes cannot answer the second question — GPUI coalesces the
//! `cx.notify()` calls of one effect cycle, so one callback may stand for any
//! number of sync-function entries. The `sync_probe` counters do: the
//! `*_sync_counts` tests below drive one gesture and assert how many times each
//! sync function actually ran. `docs/implementation/perf-baseline.md` records
//! how to run them and what the numbers were.
//!
//! The second gate is visibility (`MED-UI-02`): a panel behind another tab
//! delays its sync and pays it off when it comes back. Those tests are at the
//! end of this file — the same panels, driven with `panels::VisiblePanels`
//! published the way `WindowHost::show_tree` publishes it.

use gpui::{
    AppContext as _, Entity, ParentElement as _, Pixels, Size, Styled as _, TestAppContext,
    WindowHandle, px,
};
use gpui_component::Root;
use ravel_app::media::import::ProbedAsset;
use ravel_app::panels::{
    self, media_bin::MediaBinGpuiPanel, node_editor::NodeEditorPanel, outliner::OutlinerGpuiPanel,
    properties::PropertiesGpuiPanel, timeline::TimelineGpuiPanel,
};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::{AssetKind, AssetMetadata};
use ravel_core::id::AssetId;
use ravel_core::runtime::InvalidationHint;
use ravel_ui::layout::PanelInstanceId;

const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(800.0),
    height: px(600.0),
};

/// One instance id per panel, so a test can hide one panel and leave the rest
/// alone. The panels never interpret the id; they only ask whether it is in
/// `panels::VisiblePanels`.
const NODE_EDITOR: PanelInstanceId = PanelInstanceId(1);
const TIMELINE: PanelInstanceId = PanelInstanceId(2);
const OUTLINER: PanelInstanceId = PanelInstanceId(3);
const MEDIA_BIN: PanelInstanceId = PanelInstanceId(4);
const PROPERTIES: PanelInstanceId = PanelInstanceId(5);
const ALL_PANELS: [PanelInstanceId; 5] = [NODE_EDITOR, TIMELINE, OUTLINER, MEDIA_BIN, PROPERTIES];

/// Publish the front tabs, the way `WindowHost::show_tree` does. Anything not
/// listed is behind another tab.
fn set_visible(instances: &[PanelInstanceId], cx: &mut TestAppContext) {
    let visible = instances.iter().copied().collect();
    cx.update(|cx| cx.set_global(panels::VisiblePanels(visible)));
    cx.run_until_parked();
}

/// The five panels minus `hidden`.
fn all_but(hidden: PanelInstanceId) -> Vec<PanelInstanceId> {
    ALL_PANELS
        .iter()
        .copied()
        .filter(|instance| *instance != hidden)
        .collect()
}

/// All five panels that observe the document, so a gate removed from any one of
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
    media_bin: Entity<MediaBinGpuiPanel>,
    /// The three other mirrors, kept so a test can read what they *display*
    /// rather than only how often they synced. A sync counter cannot tell a
    /// rebuild from a counter increment, and the visibility catch-up is
    /// exactly the code where that difference matters.
    timeline: Entity<TimelineGpuiPanel>,
    outliner: Entity<OutlinerGpuiPanel>,
    node_editor: Entity<NodeEditorPanel>,
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
    open_panels_with(true, cx)
}

/// The same window with **no `VisiblePanels` global at all** — the state a
/// headless host, or the app before its first `show_tree`, is in. Nothing is
/// known to be hidden there, so every gate has to stay open.
fn open_panels_without_a_visibility_publisher(cx: &mut TestAppContext) -> Harness {
    open_panels_with(false, cx)
}

fn open_panels_with(publish_visibility: bool, cx: &mut TestAppContext) -> Harness {
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
        // Every panel starts at the front of an area: production always has a
        // publisher, and the epoch-gate tests below predate visibility.
        if publish_visibility {
            cx.set_global(panels::VisiblePanels(ALL_PANELS.into_iter().collect()));
        }
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        project
    });

    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured_in_window = captured.clone();
    let window = cx.open_window(WINDOW_SIZE, move |window, cx| {
        let panels = Panels {
            node_editor: cx.new(|cx| NodeEditorPanel::new(NODE_EDITOR, window, cx)),
            timeline: cx.new(|cx| TimelineGpuiPanel::new(TIMELINE, window, cx)),
            outliner: cx.new(|cx| OutlinerGpuiPanel::new(OUTLINER, window, cx)),
            media_bin: cx.new(|cx| MediaBinGpuiPanel::new(MEDIA_BIN, window, cx)),
            properties: cx.new(|cx| PropertiesGpuiPanel::new(PROPERTIES, window, cx)),
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
        media_bin,
        timeline,
        outliner,
        node_editor,
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

/// Import one still and hand back the id it was minted with.
fn import_still(harness: &Harness, path: &str, cx: &mut TestAppContext) -> AssetId {
    let id = harness.project.update(cx, |project, cx| {
        project
            .import_media(
                vec![ProbedAsset {
                    path: path.into(),
                    kind: AssetKind::Still,
                    metadata: AssetMetadata::default(),
                }],
                vec![],
                cx,
            )
            .imported[0]
            .0
    });
    cx.run_until_parked();
    id
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
        (
            "node_editor.refresh_from_document",
            count(PanelSync::NodeEditorRefresh),
        ),
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

    // Two moves before the measurement: the Properties target arrives through
    // the editor's republish, and the branch that adopts a *new* target rebuilds
    // its widgets in `render` instead of resolving values here. Those first
    // moves are the target settling, not the drag's steady cost, and this
    // headless harness never paints.
    drag_tick(&harness, &path, node, -2.0, cx);
    drag_tick(&harness, &path, node, -1.0, cx);

    const MOVES: u64 = 10;
    reset_syncs();
    for step in 0..MOVES {
        drag_tick(&harness, &path, node, step as f32, cx);
    }
    let counts = sync_counts("node parameter drag, 10 moves");

    // Exactly one sync per move for panels whose displayed model changes. The
    // MediaBin shows media assets, not layers, so a parameter drag is a no-op
    // for that panel and its persistent asset map stays shared.
    for (name, value) in &counts {
        if *name == "media_bin.rebuild_rows" {
            assert_eq!(*value, 0, "{name} must ignore layer parameter drags");
            continue;
        }
        assert_eq!(
            *value, MOVES,
            "{name} must resolve exactly once per move of the drag"
        );
    }
}

/// Make `layer` the Properties target, the way a click in the Outliner does.
#[cfg(debug_assertions)]
fn select_layer_target(
    comp: ravel_core::id::CompId,
    layer: ravel_core::id::LayerId,
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        cx.set_global(panels::SelectedPropertiesTarget(
            panels::PropertiesTarget::Layer {
                comp_id: comp,
                layer_id: layer,
            },
        ));
    });
    cx.run_until_parked();
}

/// Keyframe `layer`'s opacity from 0 to 1 over 30 frames, so what Properties
/// displays for it is different at every frame.
#[cfg(debug_assertions)]
fn animate_opacity(
    harness: &Harness,
    comp: ravel_core::id::CompId,
    layer: ravel_core::id::LayerId,
    cx: &mut TestAppContext,
) {
    harness.project.update(cx, |project, cx| {
        let mut curve = ravel_core::animation::curve::KeyframeCurve::new();
        curve.insert(
            0,
            0.0,
            ravel_core::animation::interpolation::Interpolation::Linear,
        );
        curve.insert(
            30,
            1.0,
            ravel_core::animation::interpolation::Interpolation::Linear,
        );
        let document = ravel_ui::document::update_layer(project.document(), comp, layer, |layer| {
            layer.opacity = ravel_core::animation::channel::AnimationChannel::keyframes(curve);
        })
        .unwrap();
        project.commit_document(document, InvalidationHint::None, cx);
    });
    cx.run_until_parked();
}

/// Advance the playhead over `frames` frames at 30 fps, the way the transport
/// publishes it.
#[cfg(debug_assertions)]
fn play(frames: u64, cx: &mut TestAppContext) {
    for frame in 0..frames {
        cx.update(|cx| {
            cx.set_global(panels::PlaybackPosition {
                frame,
                fps: ravel_core::types::FrameRate::new(30, 1),
            });
        });
        cx.run_until_parked();
    }
}

/// One second of playback at 30 fps with a *static* layer selected. Nothing
/// about the document changes and nothing on display is sampled at the
/// playhead, so no panel has anything to rebuild (`MED-UI-02`).
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
    play(FRAMES, cx);
    let counts = sync_counts("playback, 30 frames, static layer");

    // At most one: selecting a target does not itself resolve values (that
    // branch rebuilds the widgets in `render`), so the gate reopens on the
    // switch and the first playhead move is what closes it again. Every frame
    // after that costs nothing.
    assert!(
        count_of(&counts, "properties.refresh_values") <= 1,
        "a layer with no animated channel shows the same values at every frame, \
         so at most the first frame may resolve it ({} for {FRAMES} frames)",
        count_of(&counts, "properties.refresh_values")
    );
    for name in ["timeline.sync_from_project", "outliner.rebuild_rows"] {
        assert_eq!(
            count_of(&counts, name),
            0,
            "{name} mirrors the document, which playback does not change"
        );
    }
    assert_eq!(
        count_of(&counts, "media_bin.rebuild_rows"),
        0,
        "MediaBin shows media assets, so adding a layer changes nothing it displays"
    );
}

/// The other half of the skip above, and the regression it could cause: a layer
/// whose opacity is keyframed shows a different value at every frame, so
/// Properties must still resolve it once per frame. A value that visibly stops
/// moving during playback or a scrub is the failure this pins.
///
/// The static layer is selected *first* on purpose. That closes the gate, so
/// the animated layer is reached from the worst starting state — a panel that
/// has just concluded nothing follows the playhead.
#[gpui::test]
#[cfg(debug_assertions)]
fn an_animated_layer_still_follows_the_playhead(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let static_layer = add_layer(&harness, cx);
    let layer = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    cx.update(|cx| {
        cx.set_global(panels::SelectedPropertiesTarget(
            panels::PropertiesTarget::Layer {
                comp_id: comp,
                layer_id: static_layer,
            },
        ));
    });
    cx.run_until_parked();
    play(2, cx);

    animate_opacity(&harness, comp, layer, cx);
    select_layer_target(comp, layer, cx);
    cx.run_until_parked();

    const FRAMES: u64 = 30;
    reset_syncs();
    play(FRAMES, cx);
    let counts = sync_counts("playback, 30 frames, animated layer");

    assert_eq!(
        count_of(&counts, "properties.refresh_values"),
        FRAMES,
        "an animated value must be re-sampled at every frame"
    );
}

/// A layer network whose In node exposes one custom parameter driven by
/// `source`, with **no keyframe anywhere** and every shell channel left
/// constant. `custom_parameters_section` evaluates the parameter at the
/// layer-local frame, so its displayed value moves with the playhead.
#[cfg(debug_assertions)]
fn layer_with_driven_custom_parameter(
    source: ravel_core::animation::channel::ChannelSource,
) -> ravel_core::composition::Layer {
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::network as net;

    let network = Graph::new()
        .add_node(
            Node::new(NodeId::next(), net::NET_IN_TYPE_KEY)
                .with_output("amount", DataTypeId::SCALAR)
                .with_param(
                    "amount",
                    ParameterValue::Channel(AnimationChannel::new(source)),
                ),
        )
        .unwrap()
        .add_node(
            Node::new(NodeId::next(), net::NET_OUT_TYPE_KEY)
                .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
        )
        .unwrap();
    ravel_core::composition::Layer::new(ravel_core::id::LayerId::next(), "Driven", network)
        .with_time(0, 0, 100)
}

/// The gate must follow *what Properties displays*, not what the Timeline's
/// keyframe tree enumerates. `keyframes::keyframed_channel_names` drops any
/// parameter with no `Keyframes` component — correct for a tree whose purpose is
/// editing keyframes — so a parameter driven by an expression, another node's
/// output, an audio-reactive source or a blend of them produces no row at all.
/// Properties shows those values anyway, sampled at the layer-local frame.
///
/// Keying the gate on that row enumeration therefore froze them: shell channels
/// all constant plus "no rows" read as "nothing follows the playhead". Not one
/// keyframe is placed anywhere in this test on purpose — a single one would make
/// the row appear and the old implementation would pass.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_non_keyframed_driven_parameter_still_follows_the_playhead(cx: &mut TestAppContext) {
    use ravel_core::animation::channel::{
        AudioReactivePlaceholder, ChannelSource, ParameterExpression,
    };

    let sources = [
        (
            "expression",
            ChannelSource::Expression(ParameterExpression::new("frame")),
        ),
        (
            "node output",
            ChannelSource::NodeOutput(
                ravel_core::id::NodeId::next(),
                ravel_core::id::OutputPortIndex(0),
            ),
        ),
        (
            "audio reactive",
            ChannelSource::AudioReactive(AudioReactivePlaceholder::new("music")),
        ),
        (
            "blend",
            ChannelSource::Blend(
                Box::new(ChannelSource::Constant(0.0)),
                Box::new(ChannelSource::Constant(1.0)),
                ravel_core::animation::blend::BlendMode::Mix,
                0.5,
            ),
        ),
    ];

    let harness = open_panels(cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    // Closing the gate first is what makes this a real test: a panel fresh from
    // its constructor follows the playhead unconditionally.
    let static_layer = add_layer(&harness, cx);

    for (name, source) in sources {
        let layer = layer_with_driven_custom_parameter(source);
        let layer_id = layer.id;
        harness.project.update(cx, |project, cx| {
            let document = ravel_ui::document::add_layer(project.document(), comp, layer).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
        });
        cx.update(|cx| {
            cx.set_global(panels::SelectedPropertiesTarget(
                panels::PropertiesTarget::Layer {
                    comp_id: comp,
                    layer_id: static_layer,
                },
            ));
        });
        cx.run_until_parked();
        play(2, cx);

        cx.update(|cx| {
            cx.set_global(panels::SelectedPropertiesTarget(
                panels::PropertiesTarget::Layer {
                    comp_id: comp,
                    layer_id,
                },
            ));
        });
        cx.run_until_parked();

        const FRAMES: u64 = 10;
        reset_syncs();
        play(FRAMES, cx);
        let counts = sync_counts(&format!("playback, {name}-driven custom parameter"));
        assert_eq!(
            count_of(&counts, "properties.refresh_values"),
            FRAMES,
            "a {name}-driven parameter must be re-sampled at every frame, \
             even with no keyframe to give it a Timeline row"
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
    // One switch, one sync each. The switch arrives twice — as the
    // `ActiveComposition` write and as the `ProjectState` notify that follows it
    // — and whichever observer runs first absorbs the other (`MED-UI-06`).
    for name in ["timeline.sync_from_project", "outliner.rebuild_rows"] {
        assert_eq!(
            count_of(&counts, name),
            1,
            "{name} must sync once for one composition switch"
        );
    }
}

/// The hazard the composition-switch dedup introduces if it is keyed on the
/// document epoch instead of on the composition: a write to the
/// `ActiveComposition` global that carries no `ProjectState` notify must still
/// move the mirror. `panels::set_active_composition` is that write — the
/// `ProjectState` method wraps it, and this test calls the wrapped one directly.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_bare_composition_global_write_still_moves_the_mirrors(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    add_layer(&harness, cx);
    let other = harness.project.update(cx, |project, cx| {
        project.create_composition(
            ravel_ui::document::CompositionSettings::fallback("Other"),
            cx,
        )
    });
    let root = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| panels::active_composition(cx)),
        Some(other),
        "creating a composition opens it"
    );

    // No document change, no notify: only the global moves.
    let project_before = project_count(&harness, cx);
    reset_syncs();
    cx.update(|cx| panels::set_active_composition_for_tests(Some(root), cx));
    cx.run_until_parked();
    assert_eq!(
        project_count(&harness, cx),
        project_before,
        "this path must not notify `ProjectState`, or it proves nothing"
    );
    let counts = sync_counts("bare ActiveComposition write");

    for name in ["timeline.sync_from_project", "outliner.rebuild_rows"] {
        assert_eq!(
            count_of(&counts, name),
            1,
            "{name} must follow a composition switch that no notify accompanies"
        );
    }
}

/// One sync per switch, over a sequence of switches of both kinds, with a plain
/// document edit after them to prove no gate was left stuck shut.
///
/// This does **not** pin order-independence: gpui queues the `set_global` effect
/// before the `cx.notify()` that follows it, so only one delivery order is
/// reachable and this test passes whether or not the project-notify path does
/// the whole job. The post-condition that makes the order irrelevant is asserted
/// where it can be — on the helper both observers call, in
/// `outliner::tests::syncing_the_tree_adopts_the_composition_and_the_epoch`.
#[gpui::test]
#[cfg(debug_assertions)]
fn either_arrival_of_a_switch_leaves_the_other_with_nothing_to_do(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    add_layer(&harness, cx);
    let other = harness.project.update(cx, |project, cx| {
        project.create_composition(
            ravel_ui::document::CompositionSettings::fallback("Other"),
            cx,
        )
    });
    let root = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    cx.run_until_parked();

    // A full switch: the global write and the `ProjectState` notify of the same
    // change, both delivered.
    reset_syncs();
    harness.project.update(cx, |project, cx| {
        project.set_active_composition(Some(root), cx)
    });
    cx.run_until_parked();
    let counts = sync_counts("switch, both arrivals");
    assert_eq!(count_of(&counts, "outliner.rebuild_rows"), 1);
    assert_eq!(count_of(&counts, "timeline.sync_from_project"), 1);

    // The consequence of adoption: the previous composition is now a real
    // change again, so a bare global write to it must be acted on.
    reset_syncs();
    cx.update(|cx| panels::set_active_composition_for_tests(Some(other), cx));
    cx.run_until_parked();
    let counts = sync_counts("bare write back to the previous composition");
    for name in ["outliner.rebuild_rows", "timeline.sync_from_project"] {
        assert_eq!(
            count_of(&counts, name),
            1,
            "{name}: the syncing path must have adopted the composition it \
             synced for, or this write looks like a no-op"
        );
    }
    assert_eq!(
        cx.update(|cx| panels::active_composition(cx)),
        Some(other),
        "the write must have taken effect"
    );

    // And a plain document edit still reaches the tree exactly once, so the
    // epoch bookkeeping above did not leave a gate stuck shut.
    reset_syncs();
    add_layer(&harness, cx);
    let counts = sync_counts("document edit after the switches");
    for name in ["outliner.rebuild_rows", "timeline.sync_from_project"] {
        assert_eq!(count_of(&counts, name), 1, "{name}");
    }
}

/// `MED-UI-04`: `sync_from_project` no longer deep-compares the `Composition`
/// to decide whether to run — the document epoch decides, before the call. Both
/// directions have to hold, so both are asserted here: a notify that leaves the
/// epoch alone must not reach the sync, and one that moves it must.
///
/// A completed save is the notify of the first kind that actually happens (it
/// moves the window title and nothing else); an added layer is the second.
#[gpui::test]
#[cfg(debug_assertions)]
fn the_timeline_syncs_on_a_document_change_and_not_otherwise(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let dir = std::env::temp_dir().join(format!("ravel_rev_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("revision.ravprj");
    let _ = std::fs::remove_file(&path);

    add_layer(&harness, cx);

    reset_syncs();
    add_layer(&harness, cx);
    let counts = sync_counts("one document edit");
    // MediaBin shows media assets, not layers; this layer-only edit must not
    // make it rebuild. Its asset-change coverage is a separate test below.
    for name in ["timeline.sync_from_project", "outliner.rebuild_rows"] {
        assert_eq!(
            count_of(&counts, name),
            1,
            "{name}: a document change must reach the mirror exactly once"
        );
    }

    let project_before = project_count(&harness, cx);
    reset_syncs();
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
        project_count(&harness, cx) > project_before,
        "the completed save must notify project observers (window title)"
    );
    let counts = sync_counts("completed save");
    for name in [
        "timeline.sync_from_project",
        "outliner.rebuild_rows",
        "media_bin.rebuild_rows",
    ] {
        assert_eq!(
            count_of(&counts, name),
            0,
            "{name}: a notify that left the document alone must not reach the \
             mirror, so no row label is allocated"
        );
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
    let _ = std::fs::remove_dir(&dir);
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
        // MediaBin shows media assets, not layers; adding a layer leaves its
        // persistent asset map unchanged, so there is nothing to rebuild.
        if *name == "media_bin" {
            assert_eq!(after, before, "{name} must ignore a layer-only edit");
            continue;
        }
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
        // MediaBin shows media assets, not layers; add_layer does not change
        // the asset map it displays.
        if *name == "media_bin" {
            assert_eq!(after, before, "{name} must ignore a layer-only edit");
            continue;
        }
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

/// MediaBin is allowed to ignore layer-only document edits, but a media asset
/// import or deletion changes the rows it displays and must rebuild exactly
/// once for each document change.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_media_asset_change_rebuilds_media_bin_rows(cx: &mut TestAppContext) {
    let harness = open_panels(cx);

    reset_syncs();
    let plate = import_still(&harness, "/media/plate.png", cx);
    let counts = sync_counts("media asset import");
    assert_eq!(
        count_of(&counts, "media_bin.rebuild_rows"),
        1,
        "importing an asset must rebuild the MediaBin"
    );
    harness
        .media_bin
        .read_with(cx, |panel, _| assert_eq!(panel.rows().len(), 1));

    reset_syncs();
    harness.project.update(cx, |project, cx| {
        let mut document = project.document().clone();
        assert!(document.media_assets.remove(&plate).is_some());
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    cx.run_until_parked();
    let counts = sync_counts("media asset deletion");
    assert_eq!(
        count_of(&counts, "media_bin.rebuild_rows"),
        1,
        "deleting an asset must rebuild the MediaBin"
    );
    harness
        .media_bin
        .read_with(cx, |panel, _| assert!(panel.rows().is_empty()));
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

// ---------------------------------------------------------------------------
// The visibility gate (MED-UI-02)
// ---------------------------------------------------------------------------

/// A panel behind another tab has no reader, so the document edits that arrive
/// while it is back there cost it nothing.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_hidden_properties_panel_resolves_nothing(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let layer = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    select_layer_target(comp, layer, cx);

    set_visible(&all_but(PROPERTIES), cx);
    reset_syncs();
    add_layer(&harness, cx);
    add_layer(&harness, cx);
    let counts = sync_counts("two document edits, Properties hidden");

    assert_eq!(
        count_of(&counts, "properties.refresh_values"),
        0,
        "a panel behind another tab must not resolve its sections"
    );
    // The gate is per panel, not a global switch: the visible ones still work.
    for name in ["timeline.sync_from_project", "outliner.rebuild_rows"] {
        assert!(
            count_of(&counts, name) > 0,
            "{name} is still at the front of its area and must follow the edits"
        );
    }
}

/// The other half, and the reason the gate delays instead of dropping: the
/// edits skipped while hidden are resolved once when the tab comes back. Take
/// the forced sync out and this fails — the panel would keep showing the
/// values from before it was hidden until the next unrelated notify.
#[gpui::test]
#[cfg(debug_assertions)]
fn properties_returning_to_the_front_resolves_what_it_missed(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let layer = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    select_layer_target(comp, layer, cx);

    set_visible(&all_but(PROPERTIES), cx);
    add_layer(&harness, cx);
    add_layer(&harness, cx);

    reset_syncs();
    set_visible(&ALL_PANELS, cx);
    let counts = sync_counts("Properties returns to the front");

    // Once, not twice and not per skipped edit: the debt is one sync however
    // many notifications it stood for.
    assert_eq!(
        count_of(&counts, "properties.refresh_values"),
        1,
        "coming back into view must resolve the skipped edits exactly once"
    );

    // And the gate is open again afterwards: the epoch adopted by the catch-up
    // must not swallow the next real edit.
    reset_syncs();
    add_layer(&harness, cx);
    let counts = sync_counts("document edit after Properties returned");
    assert_eq!(
        count_of(&counts, "properties.refresh_values"),
        1,
        "the next edit must reach a panel that is back at the front"
    );
}

/// Playback is the notification storm `MED-UI-02` is named for: 30 frames a
/// second, each one re-resolving every section of a panel nobody can see.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_hidden_properties_panel_ignores_the_playhead(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let layer = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
    animate_opacity(&harness, comp, layer, cx);
    select_layer_target(comp, layer, cx);
    // One visible frame first, so the panel is known to be playhead-sensitive:
    // otherwise a zero below could come from the `MED-UI-02` playhead check
    // rather than from the visibility gate.
    reset_syncs();
    play(1, cx);
    assert_eq!(
        count_of(
            &sync_counts("one visible frame"),
            "properties.refresh_values"
        ),
        1,
        "the animated layer must follow the playhead while the panel is visible"
    );

    const FRAMES: u64 = 30;
    set_visible(&all_but(PROPERTIES), cx);
    reset_syncs();
    play(FRAMES, cx);
    let counts = sync_counts("playback, 30 frames, Properties hidden");
    assert_eq!(
        count_of(&counts, "properties.refresh_values"),
        0,
        "playback behind another tab must resolve nothing"
    );

    // Back at the front, it follows the playhead as before — one catch-up
    // resolve, then one per frame.
    set_visible(&ALL_PANELS, cx);
    reset_syncs();
    play(FRAMES, cx);
    let counts = sync_counts("playback, 30 frames, Properties visible again");
    assert_eq!(
        count_of(&counts, "properties.refresh_values"),
        FRAMES,
        "a panel back at the front must follow the playhead again"
    );
}

/// The gate's two halves for one panel, asserted on **what the panel shows**
/// as well as on how often it synced.
///
/// `edit` makes one change and returns whatever names it; `shows` answers
/// whether the panel's own displayed model contains that change. The counter is
/// not enough on its own: `sync_probe::record` sits at the top of each sync
/// function, so a catch-up that increments the counter and rebuilds nothing
/// would satisfy every count-based assertion here while leaving the panel
/// stale — which is the exact bug this gate can introduce.
///
/// The third step is the epoch hazard: a catch-up that recorded more than it
/// synced would leave the gate shut for the next real edit.
#[cfg(debug_assertions)]
fn assert_the_gate_delays_and_catches_up<T>(
    harness: &Harness,
    instance: PanelInstanceId,
    counter: &str,
    cx: &mut TestAppContext,
    mut edit: impl FnMut(&Harness, &mut TestAppContext) -> T,
    mut shows: impl FnMut(&Harness, &T, &mut TestAppContext) -> bool,
) {
    set_visible(&all_but(instance), cx);
    reset_syncs();
    let first = edit(harness, cx);
    let second = edit(harness, cx);
    let counts = sync_counts(&format!("{counter}, hidden"));
    assert_eq!(
        count_of(&counts, counter),
        0,
        "{counter} must not run for a panel behind another tab"
    );
    assert!(
        !shows(harness, &first, cx) && !shows(harness, &second, cx),
        "{counter}: a hidden panel must still be showing the model it had \
         before — if it already shows the edits, nothing was delayed"
    );

    reset_syncs();
    set_visible(&ALL_PANELS, cx);
    let counts = sync_counts(&format!("{counter}, back at the front"));
    assert_eq!(
        count_of(&counts, counter),
        1,
        "{counter} must resolve the skipped edits exactly once on return"
    );
    assert!(
        shows(harness, &first, cx) && shows(harness, &second, cx),
        "{counter}: the panel must display both edits it missed, not merely \
         count a sync for them"
    );

    reset_syncs();
    let third = edit(harness, cx);
    let counts = sync_counts(&format!("{counter}, edit after the return"));
    assert_eq!(
        count_of(&counts, counter),
        1,
        "{counter}: the next edit must reach a panel that is back at the front"
    );
    assert!(
        shows(harness, &third, cx),
        "{counter}: and it must be displayed"
    );
}

#[gpui::test]
#[cfg(debug_assertions)]
fn the_timeline_delays_its_mirror_while_hidden(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    assert_the_gate_delays_and_catches_up(
        &harness,
        TIMELINE,
        "timeline.sync_from_project",
        cx,
        add_layer,
        |harness, layer, cx| {
            harness
                .timeline
                .read_with(cx, |panel, _| panel.mirrors_layer(*layer))
        },
    );
}

#[gpui::test]
#[cfg(debug_assertions)]
fn the_outliner_delays_its_rows_while_hidden(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    assert_the_gate_delays_and_catches_up(
        &harness,
        OUTLINER,
        "outliner.rebuild_rows",
        cx,
        add_layer,
        |harness, layer, cx| outliner_shows_layer(harness, *layer, cx),
    );
}

#[gpui::test]
#[cfg(debug_assertions)]
fn the_media_bin_delays_its_rows_while_hidden(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let mut imported = 0;
    assert_the_gate_delays_and_catches_up(
        &harness,
        MEDIA_BIN,
        "media_bin.rebuild_rows",
        cx,
        move |harness, cx| {
            imported += 1;
            import_still(harness, &format!("/tmp/ravel_vis_{imported}.png"), cx)
        },
        |harness, asset, cx| {
            harness.media_bin.read_with(cx, |panel, _| {
                panel.rows().iter().any(|row| row.asset_id == *asset)
            })
        },
    );
}

#[gpui::test]
#[cfg(debug_assertions)]
fn the_node_editor_delays_its_graph_while_hidden(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let layer = add_layer(&harness, cx);
    let (path, node) = open_layer_network(&harness, layer, cx);
    let checked_path = path.clone();
    let mut value = 0.0;
    assert_the_gate_delays_and_catches_up(
        &harness,
        NODE_EDITOR,
        "node_editor.refresh_from_document",
        cx,
        move |harness, cx| {
            value += 1.0;
            drag_tick(harness, &path, node, value, cx);
            value
        },
        // A parameter drag *overwrites*: only the newest value is observable,
        // so "does it show this edit" is asked as "does the displayed graph
        // agree with the document" rather than as a check per value.
        |harness, _value, cx| {
            let displayed = displayed_node_value(harness, node, cx);
            displayed.is_some()
                && displayed == document_node_value(harness, &checked_path, node, cx)
        },
    );
}

/// The `value` parameter of `node` as the **document** holds it, for comparison
/// against what the panel displays.
#[cfg(debug_assertions)]
fn document_node_value(
    harness: &Harness,
    path: &ravel_ui::document::NetworkPath,
    node: ravel_core::id::NodeId,
    cx: &mut TestAppContext,
) -> Option<f32> {
    harness.project.read_with(cx, |project, _| {
        ravel_ui::document::resolve_network(project.document(), path)?
            .node(node)?
            .parameters
            .iter()
            .find(|param| param.key == "value")?
            .value
            .as_float()
    })
}

/// Whether the Outliner's own row model holds a row for `layer`.
#[cfg(debug_assertions)]
fn outliner_shows_layer(
    harness: &Harness,
    layer: ravel_core::id::LayerId,
    cx: &mut TestAppContext,
) -> bool {
    harness.outliner.read_with(cx, |panel, _| {
        panel.rows().iter().any(|row| {
            matches!(
                row.kind,
                ravel_ui::panels::outliner::OutlinerRowKind::Layer { layer: id, .. } if id == layer
            )
        })
    })
}

/// The `value` parameter of `node` **as the Node Editor displays it** — read
/// from the panel's resolved graph, not from the document.
#[cfg(debug_assertions)]
fn displayed_node_value(
    harness: &Harness,
    node: ravel_core::id::NodeId,
    cx: &mut TestAppContext,
) -> Option<f32> {
    harness.node_editor.read_with(cx, |panel, _| {
        panel
            .displayed_graph()
            .node(node)?
            .parameters
            .iter()
            .find(|param| param.key == "value")?
            .value
            .as_float()
    })
}

/// The global-driven path, and the trap in it: a composition switch that
/// arrives while the panel is hidden must be *outstanding*, not recorded.
///
/// The last step is what pins that. After the return, writing the same
/// composition again has to be a no-op — which it can only be if the catch-up
/// adopted the composition it synced for. A gate that recorded the switch on
/// the way past would leave both mirrors on the composition the user left.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_composition_switch_behind_a_tab_is_taken_up_on_the_return(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let root_layer = add_layer(&harness, cx);
    let root = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");
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

    let mirrors = ["timeline.sync_from_project", "outliner.rebuild_rows"];
    set_visible(&[NODE_EDITOR, MEDIA_BIN, PROPERTIES], cx);
    reset_syncs();
    harness.project.update(cx, |project, cx| {
        project.set_active_composition(Some(root), cx)
    });
    cx.run_until_parked();
    let counts = sync_counts("composition switch, both mirrors hidden");
    for name in mirrors {
        assert_eq!(
            count_of(&counts, name),
            0,
            "{name} must not follow a switch it cannot show"
        );
    }
    assert_eq!(
        harness
            .timeline
            .read_with(cx, |panel, _| panel.mirrored_comp()),
        Some(other),
        "the hidden Timeline must still mirror the composition it had"
    );
    assert_eq!(
        harness
            .outliner
            .read_with(cx, |panel, _| panel.mirrored_comp()),
        Some(other),
        "the hidden Outliner must not have adopted the newly active composition"
    );

    reset_syncs();
    set_visible(&ALL_PANELS, cx);
    let counts = sync_counts("both mirrors return to the front");
    for name in mirrors {
        assert_eq!(
            count_of(&counts, name),
            1,
            "{name} must take up the switch it missed"
        );
    }
    // The counts above cannot tell a rebuild from a counter increment. These
    // can: both mirrors have to be *showing* the composition switched to.
    assert_eq!(
        harness
            .timeline
            .read_with(cx, |panel, _| panel.mirrored_comp()),
        Some(root),
        "the Timeline must mirror the composition it caught up to"
    );
    assert_eq!(
        harness
            .outliner
            .read_with(cx, |panel, _| panel.mirrored_comp()),
        Some(root),
        "the Outliner must have adopted the composition it caught up to"
    );
    assert!(
        outliner_shows_layer(&harness, root_layer, cx),
        "and its rows must hold that composition's layers"
    );

    reset_syncs();
    cx.update(|cx| panels::set_active_composition_for_tests(Some(root), cx));
    cx.run_until_parked();
    let counts = sync_counts("bare write of the composition already caught up to");
    for name in mirrors {
        assert_eq!(
            count_of(&counts, name),
            0,
            "{name}: the catch-up must have adopted the composition it synced for"
        );
    }
}

/// The whole point, in one number: a drag with every panel behind a tab costs
/// the mirrors nothing at all.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_drag_behind_the_tabs_costs_every_mirror_nothing(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let layer = add_layer(&harness, cx);
    let (path, node) = open_layer_network(&harness, layer, cx);
    drag_tick(&harness, &path, node, -2.0, cx);
    drag_tick(&harness, &path, node, -1.0, cx);

    const MOVES: u64 = 10;
    set_visible(&[], cx);
    reset_syncs();
    for step in 0..MOVES {
        drag_tick(&harness, &path, node, step as f32, cx);
    }
    let counts = sync_counts("node parameter drag, 10 moves, every panel hidden");

    let total: u64 = counts.iter().map(|(_, value)| *value).sum();
    assert_eq!(
        total, 0,
        "no mirror may sync while every panel is behind a tab: {counts:?}"
    );
}

/// The Node Editor's other input is the shared layer selection, and it is
/// gated too: selecting a layer in the Outliner or the Timeline must not make
/// a background editor resolve a graph nobody can see.
///
/// `LayerSelection` is durable global state, so nothing is queued for the
/// catch-up — the selection made while hidden simply *is* the value the global
/// holds. What the hidden branch still has to do is drop `CanvasSelection`:
/// the Viewer is never gated and draws its bbox from it, so a selection left
/// pointing into the network the user walked away from would be a stale gizmo
/// on screen.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_hidden_node_editor_ignores_a_layer_selection(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let first = add_layer(&harness, cx);
    let (open_path, _node) = open_layer_network(&harness, first, cx);
    let second = add_layer(&harness, cx);
    assert!(
        !cx.update(|cx| selected_nodes_are_empty(cx)),
        "the setup must leave a node selected, or the clearing below proves nothing"
    );

    set_visible(&all_but(NODE_EDITOR), cx);
    reset_syncs();
    cx.update(|cx| panels::set_layer_selection(vec![second], cx));
    cx.run_until_parked();
    let counts = sync_counts("layer selection while the NodeEditor is hidden");

    assert_eq!(
        count_of(&counts, "node_editor.refresh_from_document"),
        0,
        "a hidden editor must not resolve the graph of a newly selected layer"
    );
    assert_eq!(
        harness
            .node_editor
            .read_with(cx, |panel, _| panel.context().cloned()),
        Some(open_path),
        "the hidden editor keeps the network it had open"
    );
    assert!(
        cx.update(|cx| selected_nodes_are_empty(cx)),
        "the node selection belongs to the network the user left: it must be \
         dropped even while hidden, because the Viewer draws from it"
    );
}

/// Whether the published `CanvasSelection` names no nodes.
#[cfg(debug_assertions)]
fn selected_nodes_are_empty(cx: &gpui::App) -> bool {
    cx.try_global::<panels::CanvasSelection>()
        .cloned()
        .unwrap_or_default()
        .nodes
        .is_empty()
}

/// The catch-up half: the selection made while hidden is applied on return,
/// with **one** document resolve rather than two.
#[gpui::test]
#[cfg(debug_assertions)]
fn a_node_editor_returning_to_the_front_opens_the_selected_network(cx: &mut TestAppContext) {
    let harness = open_panels(cx);
    let first = add_layer(&harness, cx);
    let (_open_path, _node) = open_layer_network(&harness, first, cx);
    let second = add_layer(&harness, cx);
    let comp = harness
        .project
        .read_with(cx, |project, _| project.document().root_comp)
        .expect("root comp");

    set_visible(&all_but(NODE_EDITOR), cx);
    cx.update(|cx| panels::set_layer_selection(vec![second], cx));
    cx.run_until_parked();

    reset_syncs();
    set_visible(&ALL_PANELS, cx);
    let counts = sync_counts("NodeEditor returns to a selection made behind the tab");

    assert_eq!(
        harness
            .node_editor
            .read_with(cx, |panel, _| panel.context().cloned()),
        Some(ravel_ui::document::NetworkPath::layer(comp, second)),
        "the editor must open the network the selection names now"
    );
    assert_eq!(
        count_of(&counts, "node_editor.refresh_from_document"),
        1,
        "opening the network already resolves the document: the catch-up must \
         not resolve it a second time"
    );
}

/// The fallback in `panels::is_instance_visible`: with **no `VisiblePanels`
/// global at all** nothing is known to be hidden, so every gate stays open.
///
/// A headless host and the app before its first `show_tree` are both in that
/// state, and a panel frozen by the *absence* of a publisher would show
/// nothing at all.
#[gpui::test]
#[cfg(debug_assertions)]
fn without_a_visibility_publisher_every_mirror_still_syncs(cx: &mut TestAppContext) {
    let harness = open_panels_without_a_visibility_publisher(cx);
    assert!(
        cx.update(|cx| cx.try_global::<panels::VisiblePanels>().is_none()),
        "this test is about the global being absent"
    );

    reset_syncs();
    let layer = add_layer(&harness, cx);
    let counts = sync_counts("document edit with no VisiblePanels global");
    for name in [
        "timeline.sync_from_project",
        "outliner.rebuild_rows",
        "node_editor.refresh_from_document",
    ] {
        assert_eq!(
            count_of(&counts, name),
            1,
            "{name} must follow the document when nobody publishes visibility"
        );
    }
    assert!(
        harness
            .timeline
            .read_with(cx, |panel, _| panel.mirrors_layer(layer)),
        "and the mirror must actually hold the new layer"
    );
    assert!(
        outliner_shows_layer(&harness, layer, cx),
        "and the rows must hold it too"
    );

    // Once a publisher appears, the gate behaves as everywhere else.
    set_visible(&[], cx);
    reset_syncs();
    let hidden_layer = add_layer(&harness, cx);
    let counts = sync_counts("document edit once a publisher hides everything");
    let total: u64 = counts.iter().map(|(_, value)| *value).sum();
    assert_eq!(
        total, 0,
        "a publisher that hides everything closes the gates"
    );

    set_visible(&ALL_PANELS, cx);
    assert!(
        harness
            .timeline
            .read_with(cx, |panel, _| panel.mirrors_layer(hidden_layer)),
        "and the borrow taken while hidden is still paid back"
    );
    assert!(
        outliner_shows_layer(&harness, hidden_layer, cx),
        "for the Outliner too"
    );
}

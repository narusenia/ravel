// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI integration coverage for the MediaBin panel (REQ-UI-008, media-import
//! plan unit 4). Pins the unit-4 completion conditions the headless row-model
//! tests cannot reach:
//!
//! - document assets become panel rows;
//! - selecting an asset publishes a `PropertiesTarget::MediaAsset`;
//! - "add as layer" and "new composition from asset" go through the unit-3
//!   import path;
//! - deleting an in-use asset asks first (the confirmation carries the
//!   reference count), an unused one deletes immediately, and a confirmed
//!   delete prunes the selection and the Properties target.

use gpui::{
    AnyWindowHandle, AppContext as _, Entity, ParentElement as _, Pixels, Size, Styled as _,
    TestAppContext, WindowHandle, px,
};
use gpui_component::{Root, WindowExt as _};
use ravel_app::media::import::ProbedAsset;
use ravel_app::panels::media_bin::{
    MediaBinGpuiPanel, add_asset_as_layer, delete_confirmation, new_composition_from_asset,
    request_delete_asset,
};
use ravel_app::panels::{self, PropertiesTarget};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::{AssetKind, AssetMetadata, AudioStreamMetadata, MediaAssetEntry};
use ravel_core::types::FrameRate;
use std::path::PathBuf;

const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(800.0),
    height: px(600.0),
};

struct Harness {
    window: WindowHandle<Root>,
    panel: Entity<MediaBinGpuiPanel>,
    project: Entity<ProjectState>,
}

/// Window root for the harness: the panel plus the modal layers, which the
/// host must place itself (`Root` renders the view and overlays only) —
/// without the dialog layer an opened dialog is live but unreachable.
struct TestRoot {
    panel: Entity<MediaBinGpuiPanel>,
}

impl gpui::Render for TestRoot {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
            .size_full()
            .child(self.panel.clone())
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

fn open_panel(cx: &mut TestAppContext) -> Harness {
    init_i18n();
    ravel_app::project_state::disable_background_eval_for_tests();
    let project = cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_global(panels::FocusedPanelGlobal(None));
        cx.set_global(panels::SelectedPropertiesTarget::default());
        cx.set_global(panels::MediaSelection::default());
        cx.set_global(panels::PlaybackPosition::default());
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        project
    });

    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured_in_window = captured.clone();
    let window = cx.open_window(WINDOW_SIZE, move |window, cx| {
        let panel = cx.new(|cx| MediaBinGpuiPanel::new(window, cx));
        *captured_in_window.borrow_mut() = Some(panel.clone());
        Root::new(cx.new(|_| TestRoot { panel }), window, cx)
    });
    let panel = captured
        .borrow_mut()
        .take()
        .expect("panel entity should be created");
    cx.run_until_parked();
    Harness {
        window,
        panel,
        project,
    }
}

/// A probed 2 s 1920×1080 clip at 24 fps.
fn probed_clip(path: &str) -> ProbedAsset {
    ProbedAsset {
        path: PathBuf::from(path),
        kind: AssetKind::Container,
        metadata: AssetMetadata {
            width: Some(1920),
            height: Some(1080),
            frame_rate: Some(FrameRate::new(24, 1)),
            duration_secs: Some(2.0),
            codec: Some("fake".into()),
            color_space: None,
            audio_stream_count: 1,
            // Video is stream 0, so the sole audio stream is container
            // index 1 — the number `AudioSource.stream_index` carries.
            audio_streams: vec![AudioStreamMetadata {
                stream_index: 1,
                codec: Some("fake-audio".into()),
                sample_rate: 48_000,
                channels: 2,
            }],
            file_size: 100,
        },
    }
}

fn import_clip(harness: &Harness, cx: &mut TestAppContext) {
    harness.project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/clip.mov")], vec![], cx);
    });
}

fn properties_target(cx: &TestAppContext) -> PropertiesTarget {
    cx.read(|cx| {
        cx.try_global::<panels::SelectedPropertiesTarget>()
            .cloned()
            .unwrap_or_default()
            .0
    })
}

fn has_dialog(harness: &Harness, cx: &mut TestAppContext) -> bool {
    AnyWindowHandle::from(harness.window)
        .update(cx, |_root, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

#[gpui::test]
fn document_assets_become_rows(cx: &mut TestAppContext) {
    let harness = open_panel(cx);
    import_clip(&harness, cx);
    cx.run_until_parked();

    harness.panel.read_with(cx, |panel, _| {
        assert_eq!(panel.rows().len(), 1);
        assert_eq!(panel.rows()[0].name, "clip.mov");
        assert_eq!(panel.rows()[0].duration, Some(2.0));
    });
}

#[gpui::test]
fn selecting_an_asset_publishes_the_properties_target(cx: &mut TestAppContext) {
    let harness = open_panel(cx);
    import_clip(&harness, cx);

    cx.update(|cx| {
        panels::set_media_selection(vec!["clip".to_string()], cx);
    });
    assert_eq!(
        properties_target(cx),
        PropertiesTarget::MediaAsset {
            id: "clip".to_string()
        },
    );

    // Several (or no) assets leave the panel empty — the single-asset
    // inspector is the only media target in this unit.
    cx.update(|cx| {
        panels::set_media_selection(vec!["clip".to_string(), "other".to_string()], cx);
    });
    assert_eq!(properties_target(cx), PropertiesTarget::Empty);
}

#[gpui::test]
fn add_as_layer_reuses_the_import_path(cx: &mut TestAppContext) {
    let harness = open_panel(cx);
    import_clip(&harness, cx);

    cx.update(|cx| add_asset_as_layer("clip", cx));
    cx.run_until_parked();

    harness.project.read_with(cx, |project, cx| {
        let comp = project.active_composition(cx).expect("active composition");
        assert_eq!(
            comp.layer_count(),
            2,
            "the import created one layer, the row action a second"
        );
        assert_eq!(
            project.document().media_assets.len(),
            1,
            "the asset is deduped on its resolved path, not re-registered"
        );
    });
}

#[gpui::test]
fn new_composition_from_asset_uses_the_asset_settings(cx: &mut TestAppContext) {
    let harness = open_panel(cx);
    import_clip(&harness, cx);

    cx.update(|cx| new_composition_from_asset("clip", cx));
    cx.run_until_parked();

    harness.project.read_with(cx, |project, cx| {
        let comp = project
            .active_composition(cx)
            .expect("the new composition becomes active");
        assert_eq!(comp.name, "clip");
        assert_eq!(comp.resolution, (1920, 1080));
        assert_eq!(comp.frame_rate, FrameRate::new(24, 1));
        assert_eq!(comp.duration_frames, 48, "ceil(2 s × 24 fps)");
        assert_eq!(comp.layer_count(), 1, "one layer for the asset");
        assert_eq!(project.document().compositions.len(), 2);
    });
}

#[gpui::test]
fn deleting_an_unused_asset_skips_the_confirmation(cx: &mut TestAppContext) {
    let harness = open_panel(cx);
    // Register an asset no layer references.
    harness.project.update(cx, |project, cx| {
        let doc = project
            .document()
            .clone()
            .with_media_asset_entry("plate", MediaAssetEntry::from_absolute("/media/plate.png"));
        project.commit_document(doc, ravel_core::runtime::InvalidationHint::None, cx);
    });

    AnyWindowHandle::from(harness.window)
        .update(cx, |_root, window, cx| {
            request_delete_asset("plate", window, cx);
        })
        .unwrap();

    assert!(!has_dialog(&harness, cx));
    harness.project.read_with(cx, |project, _| {
        assert!(!project.document().media_assets.contains_key("plate"));
    });
}

#[gpui::test]
fn deleting_an_in_use_asset_confirms_with_the_reference_count(cx: &mut TestAppContext) {
    let harness = open_panel(cx);
    import_clip(&harness, cx);

    // The confirmation names the referencing composition and layer and
    // carries the count.
    harness.project.read_with(cx, |project, _| {
        let message = delete_confirmation(project.document(), "clip").expect("in use");
        assert!(message.contains("(1)"), "reference count: {message}");
        assert!(message.contains("Comp 1"), "comp name: {message}");
        assert!(message.contains("clip 1"), "layer name: {message}");
    });

    cx.update(|cx| {
        panels::set_media_selection(vec!["clip".to_string()], cx);
    });
    AnyWindowHandle::from(harness.window)
        .update(cx, |_root, window, cx| {
            request_delete_asset("clip", window, cx);
        })
        .unwrap();
    assert!(has_dialog(&harness, cx), "in use asks first");
    harness.project.read_with(cx, |project, _| {
        assert!(
            project.document().media_assets.contains_key("clip"),
            "nothing is deleted before the confirmation"
        );
    });

    // Enter confirms (the alert dialog's key binding), the delete commits,
    // and the selection plus Properties target are pruned by the
    // document-change hook.
    cx.simulate_keystrokes(harness.window.into(), "enter");
    cx.run_until_parked();
    assert!(!has_dialog(&harness, cx));
    harness.project.read_with(cx, |project, _| {
        assert!(!project.document().media_assets.contains_key("clip"));
    });
    cx.read(|cx| {
        assert!(panels::media_selection(cx).is_empty());
    });
    assert_eq!(properties_target(cx), PropertiesTarget::Empty);
}

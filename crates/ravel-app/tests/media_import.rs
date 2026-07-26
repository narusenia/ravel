// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Media import path (REQ-UI-010, media-import plan unit 3).
//!
//! Pins the unit-3 completion conditions without real FFmpeg or real media
//! files (probe backends are injected):
//!
//! - an import registers assets and layers and is exactly one undo step
//!   (proved through redo, since `DocumentStore::undo` also reports `true`
//!   for reverting an uncommitted preview);
//! - a multi-file batch is one undo step, with probe failures skipped;
//! - the layer is placed at the playhead with `ceil(duration × comp_fps)`
//!   frames, falling back to the composition length when the duration is
//!   unknown;
//! - re-importing the same absolute path reuses the existing asset id.

use std::path::PathBuf;

use gpui::{AppContext as _, TestAppContext};
use ravel_app::media::import::{ImportFailure, MediaProber, ProbedAsset};
use ravel_app::panels;
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::{AssetKind, AssetMetadata, AudioStreamMetadata};
use ravel_core::graph::ParameterValue;
use ravel_core::media::{MediaError, MediaInfo, StreamInfo, VideoStreamInfo};
use ravel_core::types::FrameRate;

fn project(cx: &mut TestAppContext) -> gpui::Entity<ProjectState> {
    ravel_app::project_state::disable_background_eval_for_tests();
    cx.update(|cx| {
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        project
    })
}

fn set_playhead(cx: &mut TestAppContext, frame: u64) {
    cx.update(|cx| {
        cx.set_global(panels::PlaybackPosition {
            frame,
            fps: FrameRate::new(30, 1),
        });
    });
}

/// A probed container clip: `duration` seconds at 1920×1080, with one audio
/// stream on container index 1 (video on 0), like a muxed camera clip.
fn probed_clip(path: &str, duration: Option<f64>) -> ProbedAsset {
    ProbedAsset {
        path: PathBuf::from(path),
        kind: AssetKind::Container,
        metadata: AssetMetadata {
            width: Some(1920),
            height: Some(1080),
            frame_rate: Some(FrameRate::new(24, 1)),
            duration_secs: duration,
            codec: Some("fake".into()),
            color_space: None,
            audio_stream_count: 1,
            audio_streams: vec![AudioStreamMetadata {
                stream_index: 1,
                codec: Some("aac".into()),
                sample_rate: 48_000,
                channels: 2,
            }],
            file_size: 100,
        },
    }
}

#[gpui::test]
fn import_registers_asset_and_layer_as_one_undo_step(cx: &mut TestAppContext) {
    let project = project(cx);
    set_playhead(cx, 42);

    let summary = project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/clip.mov", Some(2.0))], vec![], cx)
    });
    assert_eq!(summary.imported.len(), 1);
    assert_eq!(summary.layers.len(), 1);
    assert!(summary.skipped.is_empty());

    project.read_with(cx, |project, _| {
        let doc = project.document();
        assert_eq!(doc.media_assets.len(), 1);
        let entry = doc
            .get_media_asset("clip")
            .expect("asset id from file stem");
        assert_eq!(entry.kind, AssetKind::Container);
        assert_eq!(entry.resolved, Some(PathBuf::from("/media/clip.mov")));
        assert_eq!(entry.metadata.audio_stream_count, 1);

        let comp = ravel_ui::document::root_composition(doc).unwrap();
        assert_eq!(comp.layer_count(), 1);
        let layer = &comp.layers[0];
        assert_eq!(layer.start_frame, 42, "the layer starts at the playhead");
        assert_eq!(layer.in_frame, 0);
        assert_eq!(
            layer.out_frame, 60,
            "out_frame = ceil(2.0 s × 30 fps comp rate)"
        );
        let media_node = layer
            .network
            .nodes()
            .find(|node| node.type_key == "media")
            .expect("media template node");
        assert!(
            media_node
                .parameters
                .iter()
                .any(|param| param.key == "asset_id"
                    && param.value == ParameterValue::String("clip".into())),
            "the media node is bound to the imported asset id"
        );
    });

    // One undo removes asset and layer together; redo proves the step was a
    // real history entry, not a preview revert.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert!(project.document().media_assets.is_empty());
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            0
        );
    });
    project.update(cx, |project, cx| assert!(project.redo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 1);
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            1
        );
    });
}

#[gpui::test]
fn a_three_file_batch_is_one_undo_step(cx: &mut TestAppContext) {
    let project = project(cx);

    project.update(cx, |project, cx| {
        project.import_media(
            vec![
                probed_clip("/media/a.mov", Some(1.0)),
                probed_clip("/media/b.mov", Some(2.0)),
                probed_clip("/media/c.mov", Some(3.0)),
            ],
            vec![],
            cx,
        )
    });
    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 3);
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            3
        );
    });

    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert!(
            project.document().media_assets.is_empty(),
            "one undo reverts the whole batch"
        );
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            0
        );
    });
    project.update(cx, |project, cx| assert!(project.redo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 3);
    });
}

/// Files that fail to probe are skipped; the successful ones still import,
/// as one undo step for the whole batch.
#[gpui::test]
fn probe_failures_are_skipped_and_successes_import(cx: &mut TestAppContext) {
    let project = project(cx);

    let prober = MediaProber::new(
        std::sync::Arc::new(|path: &std::path::Path| {
            if path.ends_with("broken.mov") {
                Err(MediaError::Other("cannot open".into()))
            } else {
                Ok(MediaInfo {
                    container: None,
                    container_name: "fake".into(),
                    streams: vec![StreamInfo::Video(VideoStreamInfo {
                        stream_index: 0,
                        codec: None,
                        codec_name: "fake".into(),
                        width: 640,
                        height: 480,
                        frame_rate: FrameRate::new(30, 1),
                        frame_count: None,
                        duration_secs: Some(1.0),
                        pixel_format: "rgba".into(),
                    })],
                    duration_secs: Some(1.0),
                })
            }
        }),
        std::sync::Arc::new(|_path| Err(MediaError::Other("no sequence".into()))),
    );

    cx.update(|cx| {
        ravel_app::media::import::import_paths_with(
            vec![
                PathBuf::from("/media/good_a.mov"),
                PathBuf::from("/media/broken.mov"),
                PathBuf::from("/media/good_b.mov"),
            ],
            prober,
            cx,
        );
    });
    cx.run_until_parked();

    project.read_with(cx, |project, _| {
        let doc = project.document();
        assert_eq!(doc.media_assets.len(), 2, "only the probing files import");
        assert!(doc.get_media_asset("good_a").is_some());
        assert!(doc.get_media_asset("good_b").is_some());
        assert!(doc.get_media_asset("broken").is_none());
    });

    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert!(project.document().media_assets.is_empty());
    });
    project.update(cx, |project, cx| assert!(project.redo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 2);
    });
}

/// A clip without a probed duration spans the whole composition (the
/// fallback keeps the layer visible instead of zero-length).
#[gpui::test]
fn unknown_duration_falls_back_to_the_composition_length(cx: &mut TestAppContext) {
    let project = project(cx);

    project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/stillish.mov", None)], vec![], cx)
    });
    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        let layer = &comp.layers[0];
        assert_eq!(layer.out_frame, comp.duration_frames);
    });
}

/// Re-importing the same absolute path reuses the asset: the id does not
/// duplicate and the asset table does not grow.
#[gpui::test]
fn reimporting_the_same_path_reuses_the_asset(cx: &mut TestAppContext) {
    let project = project(cx);

    let first = project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/clip.mov", Some(1.0))], vec![], cx)
    });
    let second = project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/clip.mov", Some(1.0))], vec![], cx)
    });

    assert_eq!(first.imported, second.imported, "same path → same asset id");
    project.read_with(cx, |project, _| {
        let doc = project.document();
        assert_eq!(doc.media_assets.len(), 1, "no duplicate asset id");
        // …but the user asked twice, so both layers exist, pointing at the
        // shared asset.
        assert_eq!(
            ravel_ui::document::root_composition(doc)
                .unwrap()
                .layer_count(),
            2
        );
    });

    // Each import is still its own undo step.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 1);
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            1
        );
    });
}

/// Importing with no active composition still registers the assets; only
/// layer creation is skipped (the batch remains one undo step).
#[gpui::test]
fn import_without_an_active_composition_skips_layers(cx: &mut TestAppContext) {
    let project = project(cx);
    project.update(cx, |project, cx| project.set_active_composition(None, cx));

    let summary = project.update(cx, |project, cx| {
        project.import_media(
            vec![probed_clip("/media/clip.mov", Some(1.0))],
            vec![ImportFailure {
                path: PathBuf::from("/media/broken.mov"),
                reason: "cannot open".into(),
            }],
            cx,
        )
    });
    assert_eq!(summary.imported.len(), 1);
    assert!(summary.layers.is_empty());
    assert_eq!(summary.skipped.len(), 1);

    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 1);
    });
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Media import and relink paths (REQ-UI-010, media-import plan units 3 and
//! 6).
//!
//! Pins their completion conditions without real FFmpeg or real media files
//! (probe backends are injected):
//!
//! - an import registers assets and **nothing else** — placing them is a
//!   separate action (refactor unit 10) — and is exactly one undo step
//!   (proved through redo, since `DocumentStore::undo` also reports `true`
//!   for reverting an uncommitted preview);
//! - a multi-file batch is one undo step, with probe failures skipped;
//! - re-importing the same absolute path reuses the existing asset id, but
//!   re-importing it after the asset was **deleted** mints a new one, so the
//!   references the deletion orphaned stay offline (`.ravprj` v9);
//! - `add_asset_layers` places at the requested frame with
//!   `ceil(duration × comp_fps)` frames, falling back to the composition
//!   length when the duration is unknown, and a whole batch is one undo
//!   step;
//! - a relink brings an offline asset back online with the new file's own
//!   metadata, in one undo step, and a file the probe refuses relinks
//!   nothing (unit 6).

use std::path::PathBuf;

use gpui::{AppContext as _, TestAppContext};
use ravel_app::media::import::{ImportFailure, MediaProber, ProbedAsset};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_core::composition::{AssetKind, AssetMetadata, AudioStreamMetadata};
use ravel_core::graph::ParameterValue;
use ravel_core::id::AssetId;
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

/// Place already-imported assets, the way every "add as layer" path does.
fn add_layers(
    project: &gpui::Entity<ProjectState>,
    ids: &[AssetId],
    start_frame: i64,
    cx: &mut TestAppContext,
) {
    project.update(cx, |project, cx| {
        project.add_asset_layers(ids, start_frame, cx);
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
fn import_registers_the_asset_without_placing_it(cx: &mut TestAppContext) {
    let project = project(cx);

    let summary = project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/clip.mov", Some(2.0))], vec![], cx)
    });
    assert_eq!(summary.imported.len(), 1);
    assert!(summary.skipped.is_empty());
    let clip = summary.imported[0].0;

    project.read_with(cx, |project, _| {
        let doc = project.document();
        assert_eq!(doc.media_assets.len(), 1);
        let entry = doc.get_media_asset(clip).expect("the imported asset");
        assert_eq!(entry.name, "clip", "display name from the file stem");
        assert_eq!(entry.kind, AssetKind::Container);
        assert_eq!(entry.resolved, Some(PathBuf::from("/media/clip.mov")));
        assert_eq!(entry.metadata.audio_stream_count, 1);

        assert_eq!(
            ravel_ui::document::root_composition(doc)
                .unwrap()
                .layer_count(),
            0,
            "importing a file does not put it on the timeline"
        );
    });

    // One undo removes the asset; redo proves the step was a real history
    // entry, not a preview revert.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert!(project.document().media_assets.is_empty());
    });
    project.update(cx, |project, cx| assert!(project.redo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 1);
    });
}

/// Placing an imported asset binds the media node to it and puts the layer at
/// the requested frame with the asset's own length — one undo step of its own,
/// separate from the import.
#[gpui::test]
fn adding_an_asset_as_a_layer_places_and_binds_it(cx: &mut TestAppContext) {
    let project = project(cx);
    let clip = project
        .update(cx, |project, cx| {
            project.import_media(vec![probed_clip("/media/clip.mov", Some(2.0))], vec![], cx)
        })
        .imported[0]
        .0;

    add_layers(&project, &[clip], 42, cx);

    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        assert_eq!(comp.layer_count(), 1);
        let layer = &comp.layers[0];
        assert_eq!(layer.start_frame, 42, "the layer starts where it was put");
        assert_eq!(layer.in_frame, 0);
        assert_eq!(
            layer.out_frame, 60,
            "out_frame = ceil(2.0 s × 30 fps comp rate)"
        );
        assert_eq!(layer.name, "clip 1", "named after the asset's file stem");
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
                    && param.value == ParameterValue::String(clip.to_param_value())),
            "the media node is bound to the imported asset id"
        );
    });

    // Undo takes the layer back and leaves the import standing.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            0
        );
        assert_eq!(project.document().media_assets.len(), 1, "still imported");
    });
    project.update(cx, |project, cx| assert!(project.redo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            1
        );
    });
}

/// Dropping several assets at once is **one** undo step: the batch is a
/// single document commit (refactor unit 10 completion condition).
#[gpui::test]
fn placing_several_assets_at_once_is_one_undo_step(cx: &mut TestAppContext) {
    let project = project(cx);
    let imported = project.update(cx, |project, cx| {
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
    let ids: Vec<AssetId> = imported.imported.iter().map(|(id, _)| *id).collect();

    add_layers(&project, &ids, 12, cx);
    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        assert_eq!(comp.layer_count(), 3);
        assert!(comp.layers.iter().all(|layer| layer.start_frame == 12));
    });

    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            0,
            "one undo reverts the whole drop"
        );
        assert_eq!(project.document().media_assets.len(), 3, "still imported");
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
    });

    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert!(
            project.document().media_assets.is_empty(),
            "one undo reverts the whole batch"
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
                        color_primaries: None,
                        color_transfer: None,
                        color_matrix: None,
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
        assert!(doc.media_asset_id_by_name("good_a").is_some());
        assert!(doc.media_asset_id_by_name("good_b").is_some());
        assert!(doc.media_asset_id_by_name("broken").is_none());
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

    let stillish = project
        .update(cx, |project, cx| {
            project.import_media(vec![probed_clip("/media/stillish.mov", None)], vec![], cx)
        })
        .imported[0]
        .0;
    add_layers(&project, &[stillish], 0, cx);
    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        let layer = &comp.layers[0];
        assert_eq!(layer.out_frame, comp.duration_frames);
    });
}

/// The failure `.ravprj` v9 exists to remove: delete an asset, import a file
/// at the same path again, and the layer the deletion orphaned must be
/// **offline** rather than silently wired to the new file.
///
/// Before v9 the asset id was the file stem, so the second import re-minted the
/// very same id and every reference the delete left behind adopted whatever was
/// now behind it — with no error and no warning. The id is minted now, so the
/// re-import is a *different* asset (`docs/implementation/asset-identity-plan.md`).
#[gpui::test]
fn deleting_an_asset_and_reimporting_the_same_path_leaves_the_old_reference_offline(
    cx: &mut TestAppContext,
) {
    let project = project(cx);

    let first = project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/clip.mov", Some(1.0))], vec![], cx)
    });
    let original = first.imported[0].0;
    add_layers(&project, &[original], 0, cx);

    // The layer's reference, captured before the asset goes away.
    let referenced = project.read_with(cx, |project, _| {
        media_reference_of_the_only_layer(project.document())
    });
    assert_eq!(referenced, Some(original));

    // Delete the asset the way the MediaBin does: drop the entry, keep the
    // layer. `ProjectState::commit_document` prunes selection, not references.
    project.update(cx, |project, cx| {
        let mut doc = project.document().clone();
        assert!(doc.media_assets.remove(&original).is_some());
        project.commit_document(doc, ravel_core::runtime::InvalidationHint::Structural, cx);
    });

    let second = project.update(cx, |project, cx| {
        project.import_media(vec![probed_clip("/media/clip.mov", Some(1.0))], vec![], cx)
    });
    let reimported = second.imported[0].0;

    assert_ne!(
        reimported, original,
        "a re-import is a new asset, never the deleted one's id"
    );
    project.read_with(cx, |project, _| {
        let doc = project.document();
        assert_eq!(
            media_reference_of_the_only_layer(doc),
            Some(original),
            "the layer still names what it always named"
        );
        assert!(
            doc.get_media_asset(original).is_none(),
            "and that asset is gone, so the layer is offline rather than \
             pointing at the file just imported"
        );
        assert!(doc.get_media_asset(reimported).is_some());
    });
}

/// The asset the document's single layer's `media` node references.
fn media_reference_of_the_only_layer(
    document: &ravel_core::composition::Document,
) -> Option<AssetId> {
    ravel_ui::document::root_composition(document)?
        .layers
        .head()?
        .network
        .nodes()
        .find_map(|node| ravel_core::composition::node_asset_reference(node))
}

/// Two different files that happen to share a file stem are two assets. The
/// id is minted, so nothing about them is shared; only the *display name* is
/// numbered apart, so the MediaBin can tell them apart until the user renames
/// one (`AID-3`).
#[gpui::test]
fn two_files_with_the_same_name_import_as_two_assets(cx: &mut TestAppContext) {
    let project = project(cx);
    let (first, second) = project.update(cx, |project, cx| {
        let summary = project.import_media(
            vec![
                probed_clip("/media/a/plate.mov", Some(1.0)),
                probed_clip("/media/b/plate.mov", Some(1.0)),
            ],
            vec![],
            cx,
        );
        (summary.imported[0].0, summary.imported[1].0)
    });
    assert_ne!(
        first, second,
        "same stem, different files, different assets"
    );

    project.read_with(cx, |project, _| {
        let doc = project.document();
        assert_eq!(doc.media_assets.len(), 2);
        let entry = |id| doc.get_media_asset(id).expect("the asset is registered");
        assert_eq!(entry(first).name, "plate");
        assert_eq!(
            entry(second).name,
            "plate 2",
            "the second one is numbered apart on screen"
        );
        assert_ne!(
            entry(first).path,
            entry(second).path,
            "and each keeps its own file"
        );
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
    });

    // …but placing it twice does make two layers pointing at the shared asset.
    let clip = first.imported[0].0;
    add_layers(&project, &[clip, clip], 0, cx);
    project.read_with(cx, |project, _| {
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            2
        );
    });

    // The second import registered nothing, so it is not a history entry:
    // one undo takes the layers away and the next takes the asset with it.
    // Since an import stopped placing layers, a re-import changes the
    // document not at all, and committing it would spend an undo on nothing.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            0,
            "the first undo reverts the placement"
        );
        assert_eq!(
            project.document().media_assets.len(),
            1,
            "the asset is still imported"
        );
    });
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert!(
            project.document().media_assets.is_empty(),
            "the next undo reverts the import itself — the re-import spent no step"
        );
    });
}

// ---------------------------------------------------------------------------
// Audio binding (audio-plan unit 4)
// ---------------------------------------------------------------------------

/// A clip with sound gets the shell's `AudioSource` bound to the same asset
/// id as the media node, with the probed **container** stream index — and it
/// is still one undo step (proved through redo).
#[gpui::test]
fn a_clip_with_audio_binds_the_shell_audio_source(cx: &mut TestAppContext) {
    let project = project(cx);

    let clip = project
        .update(cx, |project, cx| {
            project.import_media(vec![probed_clip("/media/clip.mov", Some(2.0))], vec![], cx)
        })
        .imported[0]
        .0;
    add_layers(&project, &[clip], 10, cx);

    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        let layer = &comp.layers[0];
        let audio = layer.audio.as_ref().expect("audio source on the shell");
        // The picture and the sound must name one asset: the media node's
        // reference and the shell's audio source hold the same id, spelled
        // the way each of them stores it.
        let media_asset = layer
            .network
            .nodes()
            .find(|node| node.type_key == "media")
            .and_then(|node| ravel_core::composition::node_asset_reference(node))
            .expect("the media node's asset reference");
        assert_eq!(media_asset, clip, "the media node names the imported asset");
        assert_eq!(
            audio.asset_id, clip,
            "the same asset as the media node — one asset, two consumers"
        );
        assert_eq!(audio.stream_index, 1, "the first audio stream, not video");
        assert!(!audio.audio_muted);
        // Timing stays on the shell: audio and picture share it.
        assert_eq!(layer.start_frame, 10);
        assert_eq!(layer.out_frame, 60);
        assert!(layer.has_frame_output(), "a video clip still has a picture");
    });

    // Layer and audio source revert and return together.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            0
        );
    });
    project.update(cx, |project, cx| assert!(project.redo(cx)));
    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        assert_eq!(comp.layers[0].audio.as_ref().unwrap().stream_index, 1);
    });
}

/// Silent media leaves `Layer::audio` unset — no implicit "find the audible
/// media node" resolution exists, so an unset source stays silent.
#[gpui::test]
fn silent_media_gets_no_audio_source(cx: &mut TestAppContext) {
    let project = project(cx);

    let mut silent = probed_clip("/media/silent.mov", Some(1.0));
    silent.metadata.audio_stream_count = 0;
    silent.metadata.audio_streams.clear();
    let mut still = probed_clip("/media/plate.png", None);
    still.kind = AssetKind::Still;
    still.metadata.audio_stream_count = 0;
    still.metadata.audio_streams.clear();

    let imported = project.update(cx, |project, cx| {
        project.import_media(vec![silent, still], vec![], cx)
    });
    let ids: Vec<AssetId> = imported.imported.iter().map(|(id, _)| *id).collect();
    add_layers(&project, &ids, 0, cx);

    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        assert_eq!(comp.layer_count(), 2);
        assert!(comp.layers.iter().all(|layer| layer.audio.is_none()));
    });
}

/// An audio-only container (sound, no picture) becomes a frameless audio
/// layer: no `media` node without a video stream to decode, and the shell's
/// audio source is the whole layer.
#[gpui::test]
fn an_audio_only_file_becomes_a_frameless_audio_layer(cx: &mut TestAppContext) {
    let project = project(cx);

    let mut music = probed_clip("/media/music.wav", Some(4.0));
    music.metadata.width = None;
    music.metadata.height = None;
    music.metadata.frame_rate = None;

    let music_id = project
        .update(cx, |project, cx| {
            project.import_media(vec![music], vec![], cx)
        })
        .imported[0]
        .0;
    add_layers(&project, &[music_id], 0, cx);

    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        let layer = &comp.layers[0];
        assert!(
            !layer.has_frame_output(),
            "an audio-only file has no picture to composite"
        );
        assert!(
            layer.network.nodes().all(|node| node.type_key != "media"),
            "no media node is created for a file without video"
        );
        let audio = layer.audio.as_ref().expect("audio source");
        assert_eq!(audio.asset_id, music_id);
        assert_eq!(audio.stream_index, 1);
        assert_eq!(layer.out_frame, 120, "4 s at the comp's 30 fps");
    });
}

/// An older document's metadata records only a stream count, which cannot
/// name a container stream index — such an import stays silent rather than
/// binding stream 0 (the video stream of every muxed clip).
#[gpui::test]
fn a_count_without_the_stream_list_binds_nothing(cx: &mut TestAppContext) {
    let project = project(cx);

    let mut legacy = probed_clip("/media/legacy.mov", Some(1.0));
    legacy.metadata.audio_streams.clear();
    assert_eq!(legacy.metadata.audio_stream_count, 1);

    let legacy_id = project
        .update(cx, |project, cx| {
            project.import_media(vec![legacy], vec![], cx)
        })
        .imported[0]
        .0;
    add_layers(&project, &[legacy_id], 0, cx);
    project.read_with(cx, |project, _| {
        let comp = ravel_ui::document::root_composition(project.document()).unwrap();
        assert!(comp.layers[0].audio.is_none());
    });
}

/// Without an active composition an import still registers the assets, and
/// placing them is simply a no-op.
#[gpui::test]
fn placing_without_an_active_composition_is_a_no_op(cx: &mut TestAppContext) {
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
    assert_eq!(summary.skipped.len(), 1);

    add_layers(&project, &[summary.imported[0].0], 0, cx);
    project.read_with(cx, |project, _| {
        assert_eq!(project.document().media_assets.len(), 1);
        assert_eq!(
            ravel_ui::document::root_composition(project.document())
                .unwrap()
                .layer_count(),
            0
        );
    });
}

// ---------------------------------------------------------------------------
// Relink (media-import plan unit 6)
// ---------------------------------------------------------------------------

/// A prober that opens everything except `broken.mov`, reporting 640×480 at
/// 30 fps for one second.
fn one_second_prober() -> MediaProber {
    MediaProber::new(
        std::sync::Arc::new(|path: &std::path::Path| {
            if path.ends_with("broken.mov") {
                return Err(MediaError::Other("cannot open".into()));
            }
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
                    color_primaries: None,
                    color_transfer: None,
                    color_matrix: None,
                })],
                duration_secs: Some(1.0),
            })
        }),
        std::sync::Arc::new(|_path| Err(MediaError::Other("no sequence".into()))),
    )
}

/// An asset whose variable path resolves to nothing — offline, which is the
/// state a relink exists to get out of.
fn offline_asset(project: &gpui::Entity<ProjectState>, cx: &mut TestAppContext) -> AssetId {
    let id = AssetId::next();
    project.update(cx, |project, cx| {
        let doc = project.document().clone().with_media_asset_entry(
            id,
            ravel_core::composition::MediaAssetEntry {
                name: "hero shot".into(),
                path: ravel_core::composition::AssetPath::Variable("${MEDIA}/hero.mov".into()),
                kind: AssetKind::Container,
                metadata: AssetMetadata {
                    width: Some(1920),
                    height: Some(1080),
                    ..AssetMetadata::default()
                },
                color_space: Some(ravel_core::color::ColorSpace::SRGB),
                exposed_owner: None,
                resolved: None,
            },
        );
        project.commit_document(doc, ravel_core::runtime::InvalidationHint::Structural, cx);
    });
    id
}

/// Unit 6: relinking an offline asset brings it back online with the new
/// file's own metadata, and one undo puts the offline reference back.
#[gpui::test]
fn relink_brings_an_offline_asset_online_in_one_undo_step(cx: &mut TestAppContext) {
    let project = project(cx);
    let hero = offline_asset(&project, cx);

    cx.update(|cx| {
        ravel_app::media::import::relink_asset_with(
            hero,
            PathBuf::from("/media/hero_v2.mov"),
            one_second_prober(),
            cx,
        );
    });
    cx.run_until_parked();

    project.read_with(cx, |project, _| {
        let entry = project
            .document()
            .get_media_asset(hero)
            .expect("the asset is still the same asset");
        assert!(!entry.is_offline(), "the relinked asset resolves");
        assert_eq!(
            entry.resolved,
            Some(PathBuf::from("/media/hero_v2.mov")),
            "evaluation reads the new file"
        );
        assert_eq!(
            entry.path,
            ravel_core::composition::AssetPath::Absolute("/media/hero_v2.mov".into()),
            "an unsaved project has no root to relativize against"
        );
        assert_eq!(
            (entry.metadata.width, entry.metadata.height),
            (Some(640), Some(480)),
            "the new file's numbers replace the old file's"
        );
        assert_eq!(
            entry.name, "hero shot",
            "the display name is the user's, not the file's"
        );
        assert_eq!(
            entry.color_space,
            Some(ravel_core::color::ColorSpace::SRGB),
            "an explicit input colour space describes the footage, not the file"
        );
    });

    // One step back, and redo proves it was a real history entry rather than
    // an uncommitted preview being reverted.
    project.update(cx, |project, cx| assert!(project.undo(cx)));
    project.read_with(cx, |project, _| {
        let entry = project.document().get_media_asset(hero).expect("the asset");
        assert!(entry.is_offline(), "undo restores the offline reference");
        assert_eq!(entry.metadata.width, Some(1920));
    });
    project.update(cx, |project, cx| assert!(project.redo(cx)));
    project.read_with(cx, |project, _| {
        assert!(
            !project
                .document()
                .get_media_asset(hero)
                .expect("the asset")
                .is_offline()
        );
    });
}

/// A relink is *about* one document: if File ▸ Open or New replaces it while
/// the probe runs, the result must be dropped rather than applied to whatever
/// asset now happens to carry that id.
///
/// The fresh document deliberately reuses the same `AssetId` — a project
/// reopened after a New does — so the only thing that can stop the relink is
/// the generation guard, not the "asset is gone" check.
#[gpui::test]
fn a_relink_whose_project_was_replaced_applies_to_nothing(cx: &mut TestAppContext) {
    let project = project(cx);
    let hero = offline_asset(&project, cx);

    cx.update(|cx| {
        ravel_app::media::import::relink_asset_with(
            hero,
            PathBuf::from("/media/hero_v2.mov"),
            one_second_prober(),
            cx,
        );
    });

    // The probe has not landed yet; replace the document under it.
    project.update(cx, |project, cx| {
        project.new_document(cx);
        let doc = project
            .document()
            .clone()
            .with_media_asset(hero, "/media/unrelated.mov");
        project.commit_document(doc, ravel_core::runtime::InvalidationHint::Structural, cx);
    });
    cx.run_until_parked();

    project.read_with(cx, |project, _| {
        let entry = project
            .document()
            .get_media_asset(hero)
            .expect("the new document's asset");
        assert_eq!(
            entry.resolved,
            Some(PathBuf::from("/media/unrelated.mov")),
            "the relink belonged to the document that is gone"
        );
    });
}

/// A file the probe refuses replaces nothing: the same refusal that keeps it
/// out of an import keeps it from breaking a reference that already works.
#[gpui::test]
fn a_file_the_probe_refuses_relinks_nothing(cx: &mut TestAppContext) {
    let project = project(cx);
    let hero = offline_asset(&project, cx);
    let before = project.read_with(cx, |project, _| project.document().clone());

    cx.update(|cx| {
        ravel_app::media::import::relink_asset_with(
            hero,
            PathBuf::from("/media/broken.mov"),
            one_second_prober(),
            cx,
        );
    });
    cx.run_until_parked();

    project.update(cx, |project, cx| {
        assert_eq!(project.document(), &before);
        // The only step in the history is the one that created the asset, so
        // one undo removes it outright — the refused relink pushed nothing.
        assert!(project.undo(cx));
        assert!(project.document().media_assets.is_empty());
    });
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Model files through the media import path (REQ-3D-008, `3d-scene-plan.md`
//! unit 9): a glTF / GLB / OBJ imports like any footage, and one that cannot
//! be read is **skipped with the same notice** an unprobeable clip gets
//! (`ProjectEvent::MediaImportSkipped`, shown by the workspace as a warning
//! notification). Real files, real parser: no probe backend is injected for
//! models, because they never go through FFmpeg.

use gpui::{AppContext as _, TestAppContext};
use ravel_app::media::import::{MediaProber, import_paths_with};
use ravel_app::project_state::{ProjectEvent, ProjectState, ProjectStateHandle};
use ravel_core::media::MediaError;
use std::sync::Arc;

#[derive(Default)]
struct EventLog(Vec<ProjectEvent>);

fn project(cx: &mut TestAppContext) -> gpui::Entity<ProjectState> {
    ravel_app::project_state::disable_background_eval_for_tests();
    cx.update(|cx| {
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        project
    })
}

/// A prober that fails every call, so a model that imported could only have
/// done so through its own branch.
fn refusing_prober() -> MediaProber {
    MediaProber::new(
        Arc::new(|_| Err(MediaError::Other("FFmpeg must not see a model".into()))),
        Arc::new(|_| Err(MediaError::Other("no sequence".into()))),
    )
}

fn record(project: &gpui::Entity<ProjectState>, cx: &mut TestAppContext) -> gpui::Entity<EventLog> {
    let log = cx.new(|_| EventLog::default());
    log.update(cx, |_, cx| {
        cx.subscribe(
            project,
            |log: &mut EventLog, _project, event: &ProjectEvent, _cx| log.0.push(event.clone()),
        )
        .detach();
    });
    log
}

#[gpui::test]
fn a_valid_model_imports_as_an_asset_without_touching_ffmpeg(cx: &mut TestAppContext) {
    let project = project(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tri.obj");
    std::fs::write(&path, "o tri\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();

    cx.update(|cx| import_paths_with(vec![path.clone()], refusing_prober(), cx));
    cx.run_until_parked();

    project.read_with(cx, |project, _| {
        let entries: Vec<_> = project.document().media_assets.values().collect();
        assert_eq!(entries.len(), 1, "one asset registered");
        assert_eq!(entries[0].resolved.as_deref(), Some(path.as_path()));
        assert_eq!(entries[0].name, "tri");
        assert!(entries[0].metadata.file_size > 0);
    });
}

/// The completion condition: a model that fails to load shows up in the UI the
/// way a media load failure does — the skipped-import event carries the file
/// and a reason that says it was the model that failed.
#[gpui::test]
fn a_model_that_fails_to_load_is_reported_like_a_media_failure(cx: &mut TestAppContext) {
    let project = project(cx);
    let log = record(&project, cx);
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("broken.glb");
    std::fs::write(&broken, b"glTF this is not a model").unwrap();

    cx.update(|cx| import_paths_with(vec![broken.clone()], refusing_prober(), cx));
    cx.run_until_parked();

    project.read_with(cx, |project, _| {
        assert!(
            project.document().media_assets.is_empty(),
            "a refused model registers nothing"
        );
    });
    log.read_with(cx, |log, _| {
        let failures: Vec<_> = log
            .0
            .iter()
            .filter_map(|event| match event {
                ProjectEvent::MediaImportSkipped { failures } => Some(failures),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(failures.len(), 1, "one notice for one file: {:?}", log.0);
        assert_eq!(failures[0].path, broken);
        assert!(
            failures[0].reason.contains("model load failed"),
            "the reason names the model: {}",
            failures[0].reason
        );
    });
}

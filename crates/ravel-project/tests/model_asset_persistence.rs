// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A model asset (`mesh.import`, REQ-3D-008) persists as a **project-relative**
//! reference like any footage (REQ-PROJ-001): saved inside the project it is
//! `./models/tri.obj`, and after the project folder moves the reference still
//! resolves — which is what makes the project portable.

use ravel_core::composition::{AssetPath, Document, MediaAssetEntry};
use ravel_core::id::AssetId;
use ravel_project::ProjectFile;

#[test]
fn a_model_asset_saves_relative_and_follows_the_project_folder() {
    let parent = tempfile::tempdir().unwrap();
    let home = parent.path().join("home");
    std::fs::create_dir_all(home.join("models")).unwrap();
    let model = home.join("models").join("tri.obj");
    std::fs::write(&model, "o tri\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();

    let id = AssetId::new(7);
    let document =
        Document::default().with_media_asset_entry(id, MediaAssetEntry::from_absolute(&model));
    let project_path = home.join("scene.ravprj");
    ProjectFile::from_document("Model", "2026-11-10T00:00:00Z", document)
        .save(&project_path)
        .unwrap();

    let loaded = ProjectFile::load(&project_path).unwrap();
    let entry = loaded.document.get_media_asset(id).unwrap();
    assert_eq!(entry.path, AssetPath::Relative("./models/tri.obj".into()));
    assert_eq!(entry.resolved.as_deref(), Some(model.as_path()));

    // Move the whole folder: nothing in the file names the old location.
    let moved = parent.path().join("elsewhere");
    std::fs::rename(&home, &moved).unwrap();
    let reloaded = ProjectFile::load(&moved.join("scene.ravprj")).unwrap();
    let entry = reloaded.document.get_media_asset(id).unwrap();
    assert_eq!(entry.path, AssetPath::Relative("./models/tri.obj".into()));
    let resolved = entry.resolved.as_deref().unwrap();
    assert_eq!(resolved, moved.join("models").join("tri.obj"));
    assert!(resolved.exists(), "and the file is there");
}

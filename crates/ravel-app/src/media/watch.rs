// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Watching the files behind media assets.
//!
//! A file overwritten in place keeps its path, so nothing in the document
//! says the pixels moved: without a signal the frame cache, the node cache,
//! the decoded audio and the thumbnails all keep serving the old content.
//! This module is that signal. It watches the parent directories of the
//! resolved paths and, when a file an asset reads changes, advances the
//! asset's [`MediaAssetEntry::content_revision`] through
//! [`ProjectState::advance_changed_assets`] — the revision is part of every
//! cache key, so the old content stops being reachable.
//!
//! The decision "which assets does this change touch" is a pure function of a
//! [`Document`] and a set of paths ([`changed_assets`]), so it is tested
//! without a filesystem.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Context, Entity, Subscription, Task};
use ravel_core::composition::{AssetKind, Document, MediaAssetEntry, MediaAssets};
use ravel_core::id::AssetId;

use crate::project_state::ProjectState;

/// How long the events of one write are gathered before they are acted on.
///
/// A copy or an export reports a stream of modifications; every one of them
/// would drop the caches again. Long enough to swallow the burst of a small
/// file, short enough that a finished write shows up at once.
const SETTLE: Duration = Duration::from_millis(300);

/// Whether a change at any of `changed` alters what `entry` reads.
///
/// A sequence is matched by **directory**: its frames are separate files and
/// any of them (or a new one) can change the sequence, while `resolved` names
/// only the representative frame.
fn touches(entry: &MediaAssetEntry, changed: &HashSet<PathBuf>) -> bool {
    let Some(resolved) = entry.resolved.as_deref() else {
        return false;
    };
    match entry.kind {
        AssetKind::Sequence { .. } => resolved
            .parent()
            .is_some_and(|dir| changed.iter().any(|path| path.parent() == Some(dir))),
        _ => changed.contains(resolved),
    }
}

/// The assets whose files `changed` touches.
pub fn changed_assets(document: &Document, changed: &HashSet<PathBuf>) -> HashSet<AssetId> {
    document
        .media_assets
        .iter()
        .filter(|(_, entry)| touches(entry, changed))
        .map(|(id, _)| *id)
        .collect()
}

/// `document` with the content revision of every asset `changed` touches
/// advanced by one; everything else is shared unchanged.
pub fn advance_content_revisions(document: &Document, changed: &HashSet<PathBuf>) -> Document {
    let mut document = document.clone();
    for id in changed_assets(&document, changed) {
        if let Some(entry) = document.media_assets.get_mut(&id) {
            entry.bump_content_revision();
        }
    }
    document
}

/// The directories to watch: the parent of every resolved path.
fn watch_dirs(document: &Document) -> BTreeSet<PathBuf> {
    document
        .media_assets
        .values()
        .filter_map(|entry| entry.resolved.as_deref()?.parent().map(Path::to_path_buf))
        .collect()
}

/// Owner of the live watcher. Lives as long as the workspace: a dropped
/// `RecommendedWatcher` or [`Task`] stops silently.
pub struct AssetWatch {
    tx: futures::channel::mpsc::UnboundedSender<PathBuf>,
    watcher: Option<notify::RecommendedWatcher>,
    watched: BTreeSet<PathBuf>,
    /// The map `watched` was derived from; a document edit that shares it
    /// (every layer edit) skips the walk.
    last_assets: Option<MediaAssets>,
    _observe: Subscription,
    _drain: Task<()>,
}

impl AssetWatch {
    pub fn new(project: &Entity<ProjectState>, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PathBuf>();
        let observe = cx.observe(project, |this, project, cx| {
            let document = project.read(cx).document().clone();
            this.retarget(&document);
        });
        let project_entity = project.clone();
        let project = project.downgrade();
        let drain = cx.spawn(async move |_this, cx| {
            use futures::StreamExt as _;

            while let Some(first) = rx.next().await {
                let mut changed = HashSet::from([first]);
                cx.background_executor().timer(SETTLE).await;
                while let Ok(path) = rx.try_recv() {
                    changed.insert(path);
                }
                let Some(project) = project.upgrade() else {
                    break;
                };
                cx.update(|cx| {
                    project.update(cx, |project, cx| {
                        project.advance_changed_assets(&changed, cx);
                    })
                });
            }
        });
        let mut watch = Self {
            tx,
            watcher: None,
            watched: BTreeSet::new(),
            last_assets: None,
            _observe: observe,
            _drain: drain,
        };
        // A project that already holds assets when the watch is built.
        let document = project_entity.read(cx).document().clone();
        watch.retarget(&document);
        watch
    }

    /// Point the watcher at the directories `document` reads from.
    fn retarget(&mut self, document: &Document) {
        if self
            .last_assets
            .as_ref()
            .is_some_and(|last| last.ptr_eq(&document.media_assets))
        {
            return;
        }
        self.last_assets = Some(document.media_assets.clone());
        let dirs = watch_dirs(document);
        if dirs == self.watched {
            return;
        }
        // A watch that failed (the directory is gone) is not retried until
        // the set of directories moves again; the asset is offline anyway.
        self.watcher = start_watcher(&dirs, self.tx.clone());
        self.watched = dirs;
    }
}

fn start_watcher(
    dirs: &BTreeSet<PathBuf>,
    tx: futures::channel::mpsc::UnboundedSender<PathBuf>,
) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;

    if dirs.is_empty() {
        return None;
    }
    // Some backends report canonical paths (macOS `/var` -> `/private/var`),
    // while the document holds the path the user picked; map them back so the
    // comparison in `touches` sees the document's spelling.
    let roots: Vec<(PathBuf, PathBuf)> = dirs
        .iter()
        .filter_map(|dir| Some((dir.clone(), dir.canonicalize().ok()?)))
        .collect();
    let mut watcher =
        match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            // Metadata-only changes (chmod, touch) leave the pixels alone.
            if !matches!(
                event.kind,
                notify::EventKind::Create(_)
                    | notify::EventKind::Remove(_)
                    | notify::EventKind::Modify(
                        notify::event::ModifyKind::Any
                            | notify::event::ModifyKind::Data(_)
                            | notify::event::ModifyKind::Name(_)
                            | notify::event::ModifyKind::Other
                    )
            ) {
                return;
            }
            for path in event.paths {
                let path = roots
                    .iter()
                    .find_map(|(raw, canonical)| {
                        path.strip_prefix(canonical).ok().map(|rest| raw.join(rest))
                    })
                    .unwrap_or(path);
                // The receiver is gone once the watch is dropped.
                let _ = tx.unbounded_send(path);
            }
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::warn!(%err, "could not start the media file watcher");
                return None;
            }
        };
    for dir in dirs {
        if let Err(err) = watcher.watch(dir, notify::RecursiveMode::NonRecursive) {
            tracing::warn!(dir = %dir.display(), %err, "could not watch a media directory");
        }
    }
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_with(entries: Vec<(u64, MediaAssetEntry)>) -> Document {
        entries
            .into_iter()
            .fold(Document::default(), |document, (id, entry)| {
                document.with_media_asset_entry(AssetId::new(id), entry)
            })
    }

    fn sequence(first_frame: &str) -> MediaAssetEntry {
        let mut entry = MediaAssetEntry::from_absolute(first_frame);
        entry.kind = AssetKind::Sequence {
            prefix: "f".into(),
            suffix: ".png".into(),
            padding: 4,
            start: 1,
            end: 3,
        };
        entry
    }

    fn revisions(document: &Document) -> Vec<(u64, u64)> {
        let mut all: Vec<_> = document
            .media_assets
            .iter()
            .map(|(id, entry)| (id.raw(), entry.content_revision))
            .collect();
        all.sort();
        all
    }

    /// Only the asset whose file changed advances; a sibling in the same
    /// directory, an asset elsewhere and an offline asset do not.
    #[test]
    fn only_the_changed_file_advances() {
        let mut offline = MediaAssetEntry::from_absolute("/footage/a.mov");
        offline.resolved = None;
        let document = document_with(vec![
            (1, MediaAssetEntry::from_absolute("/footage/a.mov")),
            (2, MediaAssetEntry::from_absolute("/footage/b.mov")),
            (3, MediaAssetEntry::from_absolute("/other/a.mov")),
            (4, offline),
        ]);
        let changed = HashSet::from([PathBuf::from("/footage/a.mov")]);

        let advanced = advance_content_revisions(&document, &changed);

        assert_eq!(revisions(&advanced), [(1, 1), (2, 0), (3, 0), (4, 0)]);
        assert_eq!(
            changed_assets(&document, &changed),
            HashSet::from([AssetId::new(1)])
        );
    }

    /// A sequence is one asset over many files: a change to any file in its
    /// directory advances it, a change in another directory does not.
    #[test]
    fn a_sequence_matches_by_directory() {
        let document = document_with(vec![
            (1, sequence("/shots/s1/f0001.png")),
            (2, sequence("/shots/s2/f0001.png")),
        ]);
        let changed = HashSet::from([PathBuf::from("/shots/s1/f0003.png")]);

        assert_eq!(
            revisions(&advance_content_revisions(&document, &changed)),
            [(1, 1), (2, 0)]
        );
    }

    #[test]
    fn watch_dirs_are_the_parents_of_resolved_paths() {
        let mut offline = MediaAssetEntry::from_absolute("/gone/x.mov");
        offline.resolved = None;
        let document = document_with(vec![
            (1, MediaAssetEntry::from_absolute("/footage/a.mov")),
            (2, MediaAssetEntry::from_absolute("/footage/b.mov")),
            (3, offline),
        ]);
        assert_eq!(
            watch_dirs(&document),
            BTreeSet::from([PathBuf::from("/footage")])
        );
    }
}

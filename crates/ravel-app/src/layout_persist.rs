// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Persistence of the workspace layout across sessions.
//!
//! The arrangement the user works in — every window's split/area tree, where
//! each window sits on the desktop, and which windows float above the others —
//! is written to `<config>/ravel/layout.toml` and restored at launch
//! (`LOW-APP-14`: the `WindowPlacement` field existed but nothing read or wrote
//! it). The decision logic lives in [`ravel_ui::layout_doc::LayoutStore`], which
//! is headless; this module is the file I/O and the GPUI wiring around it.
//!
//! Three rules shape the code here:
//!
//! - **A bad layout must never cost a launch.** Reading is best-effort:
//!   anything unreadable, unparsable, or stamped with a layout version this
//!   build does not know is logged and discarded, and the session starts on the
//!   built-in default. Nothing about the layout is worth failing to start over.
//! - **Writing does not block the UI.** [`save`] hands the encoded document to
//!   the background executor. The one exception is [`save_blocking`], used while
//!   the application is tearing down: there is no later frame for a spawned task
//!   to run in, so the last write is synchronous by necessity.
//! - **Someone else's project must not redecorate this workspace.** A project
//!   may embed a layout, but applying it only changes the session; the store
//!   refuses to fold a project-owned arrangement back into the application
//!   default (see `LayoutStore::capture`).

use std::path::{Path, PathBuf};

use gpui::*;
use ravel_ui::layout_doc::{LayoutDocument, LayoutStore};
use ravel_ui::shell::AppShell;
use ravel_ui::window::WindowPlacement;

/// The application-level layout document and the session's relation to it.
///
/// Durable shared application state (`.agents/rules/gpui.md`): it exists for the
/// whole process and every window's placement flows into it.
pub struct LayoutPersistence {
    store: LayoutStore,
    /// Where the document is written. `None` in a environment with no config
    /// directory (and in tests), which disables writing rather than guessing a
    /// path.
    path: Option<PathBuf>,
    /// Encoded form of the last successful write, so a save that would not
    /// change the file does no I/O at all. Layout persistence is triggered
    /// after every command, and most commands do not touch the layout.
    written: Option<String>,
}

impl Global for LayoutPersistence {}

impl LayoutPersistence {
    /// Builds the global around a restored document.
    fn new(document: Option<LayoutDocument>, path: Option<PathBuf>) -> Self {
        Self {
            store: LayoutStore::new(document.unwrap_or_default()),
            path,
            written: None,
        }
    }

    /// The layout store (read-only).
    pub fn store(&self) -> &LayoutStore {
        &self.store
    }
}

/// Reads the persisted layout document, or `None` when there is nothing usable.
///
/// Every failure — a missing file (the ordinary first-launch case), an
/// unreadable one, a corrupt one, a structurally invalid layout, or a version
/// from a newer Ravel — resolves to `None`, which the caller turns into the
/// default layout. Only the genuinely unexpected cases are logged; a file that
/// is simply not there is not a problem.
pub fn read_document(path: Option<&Path>) -> Option<LayoutDocument> {
    let path = path?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read the saved layout");
            return None;
        }
    };
    match LayoutDocument::from_toml(&text) {
        Ok(document) => Some(document),
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "ignoring the saved layout; starting on the default arrangement"
            );
            None
        }
    }
}

/// Writes `text` to `path`, creating the configuration directory if needed.
fn write_document(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

/// Installs the layout global and returns the layout the session should adopt.
///
/// Called once during bootstrap, before any window exists — reading one small
/// file synchronously here is the only way the first window can open at its
/// remembered position, and there is no UI to block yet.
///
/// `None` means "keep the shell's default arrangement": either nothing was
/// saved, or what was saved could not be used.
pub fn install(cx: &mut App) -> Option<LayoutDocument> {
    let path = crate::project::paths::global_layout_path();
    let document = read_document(path.as_deref());
    cx.set_global(LayoutPersistence::new(document.clone(), path));
    document
}

/// Restores the saved arrangement into `shell`, returning the detached windows
/// the host still has to open.
///
/// Both the layout and the user's named layouts come back; a document that
/// could not be read leaves the shell on its built-in default.
pub fn restore_into(
    shell: &mut AppShell,
    document: Option<&LayoutDocument>,
) -> Vec<ravel_ui::layout::WindowLayout> {
    let Some(document) = document else {
        return Vec::new();
    };
    for preset in &document.custom_presets {
        shell.presets_mut().save_custom(preset.clone());
    }
    shell.restore_layout(&document.layout)
}

/// Records a window's live on-desktop bounds in the layout model.
///
/// Kept out of the persistence path on purpose: this runs for every resize and
/// move, so it must not serialize or touch the disk. The model holds the fresh
/// placement, and whichever [`save`] comes next writes it.
pub fn record_placement(id: ravel_ui::window::WindowId, bounds: Bounds<Pixels>, cx: &mut App) {
    let placement = WindowPlacement {
        x: bounds.origin.x.into(),
        y: bounds.origin.y.into(),
        width: bounds.size.width.into(),
        height: bounds.size.height.into(),
    };
    let Some(session) = crate::workspace::session(cx) else {
        return;
    };
    // No `cx.notify()`: placement is not rendered, and this fires per frame
    // during a drag.
    session.update(cx, |session, _cx| {
        if let Some(window) = session.shell.layout_mut().window_mut(id) {
            window.placement = Some(placement);
        }
    });
}

/// The encoded document to write, or `None` when it would not change the file.
fn pending_write(shell: &AppShell, cx: &mut App) -> Option<(PathBuf, String)> {
    let custom: Vec<_> = shell.presets().custom_presets().cloned().collect();
    let global = cx.try_global::<LayoutPersistence>()?;
    let path = global.path.clone()?;
    let global = cx.global_mut::<LayoutPersistence>();
    global.store.capture(shell.layout(), custom);
    let text = match global.store.document().to_toml() {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(%error, "could not encode the workspace layout");
            return None;
        }
    };
    if global.written.as_deref() == Some(text.as_str()) {
        return None;
    }
    global.written = Some(text.clone());
    Some((path, text))
}

/// Persists `shell`'s current arrangement, off the UI thread.
///
/// A no-op when the document would be unchanged, which is the common case:
/// this is called after every command and most commands do not move a panel.
///
/// The shell is passed in rather than resolved from the session global because
/// the callers are inside the session entity's own update, where reading that
/// entity again would panic.
pub fn save(shell: &AppShell, cx: &mut App) {
    let Some((path, text)) = pending_write(shell, cx) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = write_document(&path, &text) {
                tracing::warn!(%error, path = %path.display(), "failed to save the workspace layout");
            }
        })
        .detach();
}

/// Persists `shell`'s current arrangement synchronously.
///
/// Used on the way out (window close, quit): a task spawned there would never
/// be polled, so the final window placement would be lost. The document is a
/// few kilobytes of TOML and the frame it delays is the last one.
pub fn save_blocking(shell: &AppShell, cx: &mut App) {
    let Some((path, text)) = pending_write(shell, cx) else {
        return;
    };
    if let Err(error) = write_document(&path, &text) {
        tracing::warn!(%error, path = %path.display(), "failed to save the workspace layout");
    }
}

/// Whether saving a project should embed the session layout into it.
pub fn embed_in_projects(cx: &App) -> bool {
    cx.try_global::<LayoutPersistence>()
        .is_some_and(|global| global.store.embed_in_projects())
}

/// Sets the embed-in-projects preference (the opt-in toggle).
pub fn set_embed_in_projects(embed: bool, cx: &mut App) {
    if cx.try_global::<LayoutPersistence>().is_none() {
        return;
    }
    cx.global_mut::<LayoutPersistence>()
        .store
        .set_embed_in_projects(embed);
}

/// The layout a project being opened should install, or `None` to leave the
/// live arrangement alone.
///
/// Recording which layout owns the session is what protects the application
/// default; see [`ravel_ui::layout_doc::LayoutStore::layout_for_project`].
pub fn layout_for_project(
    embedded: Option<&ravel_ui::layout::WorkspaceLayout>,
    cx: &mut App,
) -> Option<ravel_ui::layout::WorkspaceLayout> {
    cx.try_global::<LayoutPersistence>()?;
    cx.global_mut::<LayoutPersistence>()
        .store
        .layout_for_project(embedded)
}

/// The document a project save should embed, or `None` while the opt-in is off.
///
/// Only the layout travels: the returned document leaves the user's named
/// layouts and their own preferences at their defaults, so a shared project
/// cannot carry either.
pub fn document_for_embedding(
    layout: &ravel_ui::layout::WorkspaceLayout,
    cx: &App,
) -> Option<LayoutDocument> {
    embed_in_projects(cx).then(|| LayoutDocument::new(layout.clone()))
}

#[cfg(test)]
mod tests {
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back to
    // the built-in one so `#[test]` resolves to the real one.
    use core::prelude::v1::test;

    use super::{read_document, restore_into, write_document};
    use ravel_ui::layout_doc::LayoutStore;
    use ravel_ui::layout_doc::{LAYOUT_VERSION, LayoutDocument};
    use ravel_ui::panel::PanelKind;
    use ravel_ui::preset::BuiltinPreset;
    use ravel_ui::shell::AppShell;
    use ravel_ui::window::WindowPlacement;

    /// A saved arrangement: the Color preset in the main window, a detached
    /// window that was pinned above the others, and a placement for each.
    fn saved_document() -> LayoutDocument {
        let mut shell = AppShell::new(
            BuiltinPreset::Color,
            ravel_ui::keybindings::parser::default_bindings(),
        );
        let main = shell.layout().main_window().id;
        let viewer = shell
            .layout()
            .main_window()
            .root
            .instances()
            .into_iter()
            .find(|instance| instance.kind == PanelKind::Viewer)
            .expect("the Color preset lays out a viewer");
        let detached = shell
            .layout_mut()
            .detach_to_window(viewer.id)
            .expect("the viewer can leave the main window");
        {
            let layout = shell.layout_mut();
            layout.window_mut(main).unwrap().placement = Some(WindowPlacement {
                x: 64.0,
                y: 32.0,
                width: 1600.0,
                height: 1000.0,
            });
            let window = layout.window_mut(detached).unwrap();
            window.placement = Some(WindowPlacement {
                x: 1700.0,
                y: 120.0,
                width: 720.0,
                height: 540.0,
            });
            window.always_on_top = true;
        }
        let mut document = LayoutDocument::new(shell.layout().clone());
        shell.save_layout_as("Grading");
        document.custom_presets = shell.presets().custom_presets().cloned().collect();
        document
    }

    /// The completion criterion for restart: what a session wrote is what the
    /// next session comes up in — trees, placements, and the always-on-top pin.
    #[test]
    fn a_written_layout_is_restored_with_placement_and_always_on_top() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("layout.toml");
        let document = saved_document();
        write_document(&path, &document.to_toml().unwrap()).unwrap();

        // …the next launch reads it back and installs it into a fresh shell.
        let restored = read_document(Some(&path)).expect("the document is readable");
        let mut shell = AppShell::default();
        let opened = restore_into(&mut shell, Some(&restored));

        assert_eq!(opened.len(), 1, "the detached window is handed to the host");
        assert_eq!(
            shell.layout().main_window().root.panels(),
            document.layout.main_window().root.panels()
        );
        assert_eq!(
            shell.layout().main_window().placement,
            document.layout.main_window().placement,
            "the main window opens where it was left"
        );
        assert!(opened[0].always_on_top, "the pin survives the restart");
        assert_eq!(
            opened[0].placement,
            document.layout.windows()[1].placement,
            "so does the detached window's position"
        );
        assert!(shell.layout().is_valid());
        // The named layouts come back with it.
        assert_eq!(
            shell.presets().custom_names().collect::<Vec<_>>(),
            vec!["Grading"]
        );
    }

    /// The completion criterion for robustness: a corrupt `layout.toml` leaves
    /// the session on the default arrangement instead of stopping the launch.
    #[test]
    fn a_corrupt_layout_file_falls_back_to_the_default_arrangement() {
        let dir = tempfile::tempdir().unwrap();
        let default_panels = AppShell::default().layout().main_window().root.panels();

        let corrupt = [
            ("truncated.toml", "layout_version = 1\n[layout]\nwindows"),
            ("garbage.toml", "\u{0}\u{1}not a layout at all"),
            ("empty.toml", ""),
            ("unversioned.toml", "[layout]\nnext_window_id = 1\n"),
            (
                "future.toml",
                &format!("layout_version = {}\n[layout]\n", LAYOUT_VERSION + 1),
            ),
        ];
        for (name, contents) in corrupt {
            let path = dir.path().join(name);
            std::fs::write(&path, contents).unwrap();

            let restored = read_document(Some(&path));
            assert!(restored.is_none(), "{name} must be discarded");

            let mut shell = AppShell::default();
            let opened = restore_into(&mut shell, restored.as_ref());
            assert!(opened.is_empty(), "{name}");
            assert_eq!(
                shell.layout().main_window().root.panels(),
                default_panels,
                "{name} must leave the default arrangement in place"
            );
            assert!(shell.layout().is_valid(), "{name}");
        }
    }

    /// The arrangement a layout describes: which panels sit in which window,
    /// where each window is, and which ones float. Instance *ids* are excluded
    /// on purpose — installing a layout reassigns them
    /// ([`ravel_ui::layout::WorkspaceLayout::adopt`]) so a pane's cached view
    /// can never be handed to a different panel, and that reassignment is not a
    /// change to the arrangement.
    fn arrangement(
        layout: &ravel_ui::layout::WorkspaceLayout,
    ) -> Vec<(Vec<PanelKind>, Option<WindowPlacement>, bool)> {
        layout
            .windows()
            .iter()
            .map(|window| (window.root.panels(), window.placement, window.always_on_top))
            .collect()
    }

    /// The completion criterion for embedding: alternating between a project
    /// that ships a layout and one that does not must leave the application
    /// default — what the next launch restores — exactly as the user left it.
    #[test]
    fn alternating_projects_never_dirty_the_saved_application_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.toml");

        // The user's own arrangement, saved as it would be at the end of a
        // session.
        let app_default = saved_document();
        let mut shell = AppShell::default();
        restore_into(&mut shell, Some(&app_default));
        let mut store = LayoutStore::new(app_default.clone());
        store.capture(
            shell.layout(),
            shell.presets().custom_presets().cloned().collect(),
        );
        write_document(&path, &store.document().to_toml().unwrap()).unwrap();
        let expected = arrangement(store.app_layout());

        // A project that ships its own arrangement.
        let embedded = LayoutDocument::new(
            AppShell::new(
                BuiltinPreset::Motion,
                ravel_ui::keybindings::parser::default_bindings(),
            )
            .layout()
            .clone(),
        );
        let embedded_arrangement = arrangement(&embedded.layout);

        for round in 0..3 {
            for (project, expect_session) in [
                (Some(&embedded.layout), Some(&embedded_arrangement)),
                (None, Some(&expected)),
            ] {
                if let Some(session) = store.layout_for_project(project) {
                    shell.restore_layout(&session);
                    assert_eq!(
                        arrangement(shell.layout()),
                        *expect_session.unwrap(),
                        "round {round}: the session runs on the installed layout"
                    );
                }
                // The user keeps working, and the session is folded back in
                // after every command.
                store.capture(
                    shell.layout(),
                    shell.presets().custom_presets().cloned().collect(),
                );
                write_document(&path, &store.document().to_toml().unwrap()).unwrap();
            }
            // The application default is untouched, both in memory and on disk.
            assert_eq!(arrangement(store.app_layout()), expected, "round {round}");
            let reloaded = read_document(Some(&path)).expect("still readable");
            assert_eq!(arrangement(&reloaded.layout), expected, "round {round}");
            assert_eq!(
                reloaded.custom_presets, app_default.custom_presets,
                "round {round}: the user's named layouts survive too"
            );
        }
    }

    /// A first launch has no file at all, which is not an error and not logged
    /// as one.
    #[test]
    fn a_missing_layout_file_is_the_ordinary_first_launch() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_document(Some(&dir.path().join("absent.toml"))).is_none());
        assert!(read_document(None).is_none());
    }

    /// A placement that cannot describe a real window is rejected before it
    /// reaches the platform, so a hand-edited file cannot open a window the
    /// user is unable to find.
    #[test]
    fn a_degenerate_saved_placement_is_not_usable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.toml");
        let mut document = saved_document();
        let main = document.layout.main_window().id;
        document.layout.window_mut(main).unwrap().placement = Some(WindowPlacement {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
        write_document(&path, &document.to_toml().unwrap()).unwrap();

        let restored = read_document(Some(&path)).expect("the document still parses");
        assert!(
            !restored.layout.main_window().placement.unwrap().is_usable(),
            "the host must fall back to its default size"
        );
    }
}

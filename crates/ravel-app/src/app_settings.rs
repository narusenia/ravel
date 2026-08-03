// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The one path from persisted settings to running behaviour
//! (`docs/implementation/settings-screen-plan.md`, unit `SET-1`; closes the
//! core of `MED-APP-10`).
//!
//! Four layers merge into one value (`default → global → project → user`,
//! [`crate::project::settings`]). This module is where that resolved value
//! **reaches the application**: it is held in a single durable global
//! ([`AppSettings`]), and every consumer reads it from there. Nothing writes a
//! setting straight into the subsystem it configures, so there is exactly one
//! place where "what the file says" becomes "what the app does":
//!
//! ```text
//! <config>/ravel/settings.toml ─┐
//!                               ├→ AppSettings (Global) → ravel_i18n::set_locale
//! .ravprj settings.toml ────────┘                       → gpui_component::Theme
//!                                                       → (SET-8) cache budget
//! ```
//!
//! Three rules shape the code here:
//!
//! - **A bad settings file must never cost a launch.** A missing, unreadable,
//!   or malformed global layer resolves to no overrides at all — logged, then
//!   the defaults. The same for a locale the catalogs do not contain: it is a
//!   warning and a fallback, never a failed start.
//! - **A write touches one layer.** [`update`] persists the layer it edited
//!   and nothing else: the global layer goes to
//!   `<config>/ravel/settings.toml`, the project layer travels with the
//!   `.ravprj` the document is saved into. Editing one can never rewrite the
//!   other's file.
//! - **A failed write is visible.** The global layer is written off the UI
//!   thread; a failure becomes
//!   [`ProjectEvent::SettingsSaveFailed`](crate::project_state::ProjectEvent::SettingsSaveFailed),
//!   which the workspace turns into a notification. Silent settings loss is
//!   the shape of `CRIT-02`.
//!
//! The `user` layer has no store and no writer (plan: it is deliberately kept
//! free as the slot for a machine-local or CLI override), so resolution here
//! layers `default → global → project`.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, Global};
use gpui_component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};

use crate::project::atomic_write;
use crate::project::paths;
use crate::project::settings::{AppearanceMode, ResolvedSettings, SettingsLayer};
use crate::project_state::ProjectStateHandle;

/// The locale Ravel starts in and falls back to. `assets/locales/en.toml` is
/// the catalog every other one falls back to key-by-key
/// ([`ravel_i18n::translate`]), so it is also the only locale a launch may
/// depend on.
pub const DEFAULT_LOCALE: &str = "en";

/// Which layer an edit is written to.
///
/// The screen that owns the edit picks the scope — Preferences write the
/// global layer, Project Settings the project layer — so no setting needs a
/// per-row layer choice and no edit can land in the wrong file by accident
/// (plan: 環境設定とプロジェクト設定の 2 画面に分ける).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsScope {
    /// `<config_base>/ravel/settings.toml` — per-user preferences that
    /// outlive any project.
    Global,
    /// The `settings.toml` entry of the open `.ravprj`, which overrides the
    /// global layer (REQ-PROJ-004).
    Project,
}

/// The settings in force, plus the layers they were resolved from.
///
/// Durable shared application state (`.agents/rules/gpui.md`): it exists for
/// the whole process, changes rarely, and is read by whoever renders next. It
/// is not an event channel — nothing is parked here for another entity to
/// consume and clear.
pub struct AppSettings {
    /// Overrides from `<config>/ravel/settings.toml`.
    global: SettingsLayer,
    /// Overrides from the open project. Replaced wholesale when a document is
    /// opened or created ([`set_project_layer`]).
    project: SettingsLayer,
    /// `default → global → project`, with the built-in defaults applied.
    resolved: ResolvedSettings,
    /// Where the global layer is written. `None` when the platform has no
    /// config directory (a headless environment without `HOME`), which
    /// disables writing rather than guessing a path — the same treatment
    /// [`crate::layout_persist`] gives the layout file.
    global_path: Option<PathBuf>,
    /// The write in flight, if any. Each new write awaits it before publishing
    /// so the file ends on the newest value ([`write_global_layer`]).
    write_chain: Option<gpui::Task<()>>,
}

impl Global for AppSettings {}

impl AppSettings {
    /// The settings in force.
    pub fn resolved(&self) -> &ResolvedSettings {
        &self.resolved
    }

    /// The explicit overrides `scope` holds. A field that is `None` here is
    /// not overridden by that layer, which is what the settings dialog shows
    /// as "not customized" and what its reset control restores
    /// (`gpui_component::setting`'s `on_reset`, unit `SET-2`).
    pub fn layer(&self, scope: SettingsScope) -> &SettingsLayer {
        match scope {
            SettingsScope::Global => &self.global,
            SettingsScope::Project => &self.project,
        }
    }

    /// Re-resolve after a layer changed; returns what moved.
    fn reresolve(&mut self) -> Changed {
        let previous = std::mem::replace(
            &mut self.resolved,
            ResolvedSettings::from_layers(&[self.global.clone(), self.project.clone()]),
        );
        Changed {
            locale: previous.locale != self.resolved.locale,
            appearance: previous.theme_mode != self.resolved.theme_mode
                || previous.light_theme != self.resolved.light_theme
                || previous.dark_theme != self.resolved.dark_theme,
        }
    }

    /// The global layer encoded for its file.
    ///
    /// The three outcomes are kept apart because they are not the same event:
    /// there is nowhere to write (no config directory), the layer could not be
    /// encoded (which loses the edit and must be reported), or here is the file
    /// and the bytes.
    fn pending_global_write(&self) -> PendingWrite {
        let Some(path) = self.global_path.clone() else {
            return PendingWrite::NoTarget;
        };
        match self.global.to_toml() {
            Ok(text) => PendingWrite::Ready { path, text },
            Err(error) => PendingWrite::EncodeFailed {
                path,
                error: error.to_string(),
            },
        }
    }
}

/// Which resolved values a re-resolution moved.
///
/// Applying a setting is not free — a locale switch redraws every window and a
/// theme switch rebuilds the palette — so each subsystem is re-applied only when
/// the value it consumes actually changed. An edit to a playback setting must
/// not repaint the UI.
#[derive(Clone, Copy, Debug)]
struct Changed {
    locale: bool,
    appearance: bool,
}

impl Changed {
    /// Everything, for the initial publication: nothing has been applied yet,
    /// so there is no previous value to compare against.
    const ALL: Self = Self {
        locale: true,
        appearance: true,
    };
}

/// What [`AppSettings::pending_global_write`] found.
enum PendingWrite {
    /// The global layer has no file (a platform with no config directory).
    NoTarget,
    /// The layer is encoded and ready for its file.
    Ready { path: PathBuf, text: String },
    /// The layer could not be encoded, so the edit reaches no file. Reported
    /// like any other failed write: the user changed a setting Ravel then
    /// failed to keep.
    EncodeFailed { path: PathBuf, error: String },
}

// ===========================================================================
// Startup
// ===========================================================================

/// The global settings layer as read at launch, and the file it came from.
///
/// Read before the GPUI application exists, because the locale has to be
/// active before the first translated string is produced; [`install`] then
/// publishes the same values as the global.
#[derive(Clone, Debug, Default)]
pub struct GlobalSettingsFile {
    layer: SettingsLayer,
    path: Option<PathBuf>,
}

impl GlobalSettingsFile {
    /// The settings in force before any project is open (`default → global`).
    pub fn resolved(&self) -> ResolvedSettings {
        ResolvedSettings::from_layers(std::slice::from_ref(&self.layer))
    }
}

/// Read the global settings layer from its platform location.
pub fn read_global_settings() -> GlobalSettingsFile {
    read_global_settings_at(paths::global_settings_path())
}

/// [`read_global_settings`] against an explicit path (tests, and any future
/// `--config` override).
pub fn read_global_settings_at(path: Option<PathBuf>) -> GlobalSettingsFile {
    let layer = path.as_deref().map(read_layer).unwrap_or_default();
    GlobalSettingsFile { layer, path }
}

/// Read one settings layer, degrading to no overrides.
///
/// A file that is simply not there is the ordinary first launch and is not
/// logged; anything else is a warning, because a settings file the user
/// edited by hand and got wrong must not silently look empty forever.
fn read_layer(path: &Path) -> SettingsLayer {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SettingsLayer::default();
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read the settings file");
            return SettingsLayer::default();
        }
    };
    match SettingsLayer::from_toml(&text) {
        Ok(layer) => layer,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "ignoring the settings file; starting on the defaults"
            );
            SettingsLayer::default()
        }
    }
}

/// Publish the resolved settings as the application global.
///
/// Called once during bootstrap with the layer [`read_global_settings`]
/// produced, so the file is read exactly once per launch.
pub fn install(file: GlobalSettingsFile, cx: &mut App) {
    let mut settings = AppSettings {
        global: file.layer,
        project: SettingsLayer::default(),
        resolved: ResolvedSettings::default(),
        global_path: file.path,
        write_chain: None,
    };
    settings.reresolve();
    cx.set_global(settings);
    // Everything the settings configure is applied once here. For the locale
    // that also reconciles the published value with the one that actually took
    // effect (it was activated before the application existed,
    // [`apply_startup_locale`]), so a settings file naming an unknown locale
    // does not leave the global claiming it.
    apply(Changed::ALL, cx);
    observe_theme_registry(cx);
}

/// Re-apply the appearance whenever the set of available themes changes.
///
/// The themes directory is read **asynchronously** and re-read on every file
/// change, so the theme a settings file names may not exist yet when the
/// appearance is first applied — and `gpui_component`'s own registry observer
/// cannot recover from that: it re-resolves the slots from the names the `Theme`
/// currently holds, which after a fallback are the fallback's names, not the
/// requested ones. Without this observer a theme file that appears (or is
/// renamed back) after startup would never be worn, and the requested name the
/// resolved settings deliberately keep would have nothing to act on.
///
/// This cannot loop: the appearance is applied by writing `Theme`, never
/// `ThemeRegistry`.
fn observe_theme_registry(cx: &mut App) {
    cx.observe_global::<ThemeRegistry>(apply_resolved_appearance)
        .detach();
}

/// What the startup locale decision did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocaleOutcome {
    /// `locale` is the active one.
    Applied(String),
    /// The requested locale could not be loaded, so the previously active one
    /// stays (at startup that is [`DEFAULT_LOCALE`]). Logged as a warning.
    FellBack { requested: String, error: String },
    /// No catalog could be loaded at all; `t!` yields raw keys. The launch
    /// still continues — an unreadable `assets/locales` is a broken
    /// installation, not a reason to refuse to open the user's work.
    Unavailable { error: String },
}

/// Load the locale catalogs and activate `requested`.
///
/// Two steps rather than `init(dir, requested)` on purpose: [`ravel_i18n::init`]
/// fails outright when its default locale is missing, so passing a locale that
/// came from a settings file would let one bad line leave the application with
/// **no** catalogs. Initializing on [`DEFAULT_LOCALE`] keeps the fallback
/// catalog as the floor, and [`ravel_i18n::set_locale`] — the same primitive
/// the language switch will use (`SET-4`) — reports an unknown locale as a
/// recoverable error. Startup and runtime therefore agree on what a valid
/// locale is.
pub fn apply_startup_locale(locale_dir: &Path, requested: &str) -> LocaleOutcome {
    if let Err(error) = ravel_i18n::init(locale_dir, DEFAULT_LOCALE) {
        tracing::error!(
            %error,
            dir = %locale_dir.display(),
            "could not load any locale catalog; UI text falls back to raw keys"
        );
        return LocaleOutcome::Unavailable {
            error: error.to_string(),
        };
    }
    apply_locale(requested)
}

/// Activate `locale`, keeping the active one when it cannot be loaded.
///
/// [`ravel_i18n::set_locale`] only switches on success, so a rejected locale
/// leaves the store untouched — the caller does not have to restore anything.
fn apply_locale(locale: &str) -> LocaleOutcome {
    if ravel_i18n::current_locale() == locale {
        return LocaleOutcome::Applied(locale.to_string());
    }
    match ravel_i18n::set_locale(locale) {
        Ok(()) => LocaleOutcome::Applied(locale.to_string()),
        Err(error) => {
            tracing::warn!(
                %error,
                requested = locale,
                active = %ravel_i18n::current_locale(),
                "unknown locale in settings; keeping the active one"
            );
            LocaleOutcome::FellBack {
                requested: locale.to_string(),
                error: error.to_string(),
            }
        }
    }
}

// ===========================================================================
// Reading
// ===========================================================================

/// The settings in force.
///
/// Defaults when the global is not installed, which is the case in tests and
/// in any tool that builds panels without the application bootstrap — a
/// consumer never has to special-case its absence.
pub fn resolved(cx: &App) -> ResolvedSettings {
    cx.try_global::<AppSettings>()
        .map(|settings| settings.resolved.clone())
        .unwrap_or_default()
}

/// The explicit overrides `scope` holds; see [`AppSettings::layer`].
pub fn layer(scope: SettingsScope, cx: &App) -> SettingsLayer {
    cx.try_global::<AppSettings>()
        .map(|settings| settings.layer(scope).clone())
        .unwrap_or_default()
}

// ===========================================================================
// Writing
// ===========================================================================

/// Change one setting in `scope`, apply the result, and persist that layer.
///
/// `edit` receives the layer being written, so a caller states exactly the
/// field it owns and nothing else:
///
/// ```ignore
/// // Preferences ▸ Language
/// app_settings::update(SettingsScope::Global, |l| l.locale = Some("ja".into()), cx);
/// // "Reset to default": drop the override, so the value falls back to the
/// // layers below (`gpui_component::setting`'s `on_reset`).
/// app_settings::update(SettingsScope::Global, |l| l.locale = None, cx);
/// ```
///
/// The whole layer is never handed in as a value: an edit must not be able to
/// discard overrides it does not know about (a build that adds a field would
/// otherwise erase it from an older caller's write).
///
/// Persistence differs per scope, because the two layers live in different
/// files:
///
/// - [`SettingsScope::Global`] is written to `<config>/ravel/settings.toml`
///   immediately, atomically, and off the UI thread.
/// - [`SettingsScope::Project`] belongs to the `.ravprj` and is written by the
///   next project save, which is also what makes the project dirty — settings
///   the user changed and did not save are unsaved changes like any other.
///
/// Call this from an app or view context, never from inside a
/// [`crate::project_state::ProjectState`] update: both the dirty mark and the
/// failure report update that entity, and gpui forbids nesting an update of the
/// entity already being updated.
pub fn update(scope: SettingsScope, edit: impl FnOnce(&mut SettingsLayer), cx: &mut App) {
    if cx.try_global::<AppSettings>().is_none() {
        tracing::warn!("settings edit dropped: the settings global is not installed");
        return;
    }
    let settings = cx.global_mut::<AppSettings>();
    match scope {
        SettingsScope::Global => edit(&mut settings.global),
        SettingsScope::Project => edit(&mut settings.project),
    }
    let changed = settings.reresolve();
    let write = match scope {
        SettingsScope::Global => settings.pending_global_write(),
        // The project layer travels with the `.ravprj`; the next save writes it.
        SettingsScope::Project => PendingWrite::NoTarget,
    };

    apply(changed, cx);
    match write {
        PendingWrite::Ready { path, text } => write_global_layer(path, text, cx),
        PendingWrite::EncodeFailed { path, error } => {
            tracing::error!(
                %error,
                path = %path.display(),
                "could not encode the global settings layer"
            );
            report_write_failure(path, error, cx);
        }
        PendingWrite::NoTarget => {
            if scope == SettingsScope::Project {
                mark_project_dirty(cx);
            }
        }
    }
}

/// Adopt the settings layer of the project the session just opened or
/// created, re-resolving in force.
///
/// The one entry point for the project layer, called from the document
/// replacement path ([`crate::project_state::ProjectState`]) so a project's
/// own overrides take effect as it opens and stop applying as it closes.
pub fn set_project_layer(layer: SettingsLayer, cx: &mut App) {
    if cx.try_global::<AppSettings>().is_none() {
        return;
    }
    let settings = cx.global_mut::<AppSettings>();
    if settings.project == layer {
        return;
    }
    settings.project = layer;
    let changed = settings.reresolve();
    apply(changed, cx);
}

/// Put the values that moved in force.
///
/// The single place a resolved value reaches the subsystem it configures, so
/// there is one answer to "what happens when this setting changes" no matter
/// which layer moved it — a project opening, a preferences edit, or startup.
fn apply(changed: Changed, cx: &mut App) {
    if changed.locale {
        apply_resolved_locale(cx);
    }
    if changed.appearance {
        apply_resolved_appearance(cx);
    }
}

/// Activate the resolved locale and record the one that actually took effect.
///
/// A locale the catalogs reject leaves the previously active one running
/// ([`apply_locale`]), so the resolved value has to be corrected to name it —
/// otherwise the two disagree, and the language control would offer a locale
/// the UI is not written in. Opening a project whose layer names an unknown
/// locale therefore keeps the language the user was already reading rather
/// than snapping to it, and the settings say so.
fn apply_resolved_locale(cx: &mut App) {
    let requested = cx.global::<AppSettings>().resolved.locale.clone();
    let outcome = apply_locale(&requested);
    // Every translated string is produced inside a `render`, from a process-wide
    // catalog that belongs to no entity — so the way a language change reaches
    // the screen is a redraw of what is open, not a notification from a value
    // that changed. One refresh covers every panel, every dialog (the settings
    // dialog included) and every window, without each of the sixteen panel types
    // having to subscribe to settings it does not otherwise read. The menu bar
    // lives outside the element tree and is rebuilt by the session's observer
    // (`crate::workspace::RavelWorkspace`).
    cx.refresh_windows();
    if let LocaleOutcome::Applied(_) = outcome {
        return;
    }
    let effective = ravel_i18n::current_locale();
    if effective.is_empty() {
        // No catalog was ever loaded (a tool that never called
        // `ravel_i18n::init`), so there is no locale in force to name and the
        // resolved value keeps what the file asked for.
        return;
    }
    cx.global_mut::<AppSettings>().resolved.locale = effective;
}

/// Put the resolved appearance in force: the theme mode, and the two themes it
/// chooses between.
///
/// Called at startup and whenever an appearance value moves, and again when the
/// theme registry has reloaded — the themes directory is read asynchronously, so
/// a theme this names may simply not exist yet when the settings are first
/// applied ([`crate::main`]'s themes loader passes this as its `on_load`).
///
/// The themes are handed over **as registry entries**, never as colours copied
/// out of one: `gpui_component`'s registry observer re-resolves
/// `Theme::light_theme` / `dark_theme` by *name* on every reload, which is what
/// makes editing a theme file hot-reload. Writing colours straight into the
/// theme would be undone by the next reload and would take that behaviour with
/// it.
///
/// A name no theme in the registry carries falls back to the registry's own
/// default for that mode, with a warning: a settings file may say anything, and
/// a renamed or deleted theme file must not cost a launch. The resolved settings
/// keep naming the theme that was asked for (unlike the locale, which is
/// corrected to the one in force) precisely because the registry fills in late —
/// forgetting the request would mean a custom theme never applied.
pub fn apply_resolved_appearance(cx: &mut App) {
    if !cx.has_global::<ThemeRegistry>() || !cx.has_global::<Theme>() {
        // A tool that builds views without `gpui_component::init`: there is no
        // theme to configure, which is not an error here.
        tracing::debug!("appearance not applied: gpui_component is not initialized");
        return;
    }
    let settings = resolved(cx);
    let light = theme_named(&settings.light_theme, ThemeMode::Light, cx);
    let dark = theme_named(&settings.dark_theme, ThemeMode::Dark, cx);
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    match settings.theme_mode {
        AppearanceMode::System => Theme::sync_system_appearance(None, cx),
        AppearanceMode::Light => Theme::change(ThemeMode::Light, None, cx),
        AppearanceMode::Dark => Theme::change(ThemeMode::Dark, None, cx),
    }
    // `Theme::change` only refreshes the window it is given one of, and this is
    // an app-level change: every open window is now painting stale colours.
    cx.refresh_windows();
}

/// The registry entry called `name`, falling back for a name no theme carries.
///
/// The fallback is Ravel's own bundled theme for that mode before the
/// registry's built-in default, because those two are not equally good answers:
/// the bundled theme is what an unset setting resolves to, so a renamed or
/// deleted theme file leaves the user looking at the Ravel they know rather than
/// at gpui-component's stock palette. The registry's default is the last resort
/// for a launch with no themes directory at all.
fn theme_named(name: &str, mode: ThemeMode, cx: &App) -> Rc<ThemeConfig> {
    let registry = ThemeRegistry::global(cx);
    if let Some(config) = registry.themes().get(name) {
        return config.clone();
    }
    let bundled = match mode {
        ThemeMode::Light => crate::project::settings::DEFAULT_LIGHT_THEME,
        ThemeMode::Dark => crate::project::settings::DEFAULT_DARK_THEME,
    };
    if let Some(config) = registry.themes().get(bundled) {
        tracing::warn!(
            requested = name,
            using = bundled,
            "no theme by that name; using the bundled one for this mode"
        );
        return config.clone();
    }
    let fallback = match mode {
        ThemeMode::Light => registry.default_light_theme(),
        ThemeMode::Dark => registry.default_dark_theme(),
    };
    tracing::warn!(
        requested = name,
        using = %fallback.name,
        "neither that theme nor the bundled one is loaded; using the registry default"
    );
    fallback.clone()
}

/// Write the global layer off the UI thread, reporting a failure as a
/// notification event.
///
/// Writes are **chained**, not merely spawned: each one awaits the previous
/// write before it publishes, so the file ends up holding the last edit the
/// user made. Independent background tasks would not promise that — two edits
/// in quick succession could rename in either order and leave the older value
/// on disk while the global holds the newer one. The chain lives in
/// [`AppSettings::write_chain`], which also keeps the task alive without
/// detaching it.
///
/// The encoding and the write itself stay off the UI thread; only the await
/// and the failure report run on it.
fn write_global_layer(path: PathBuf, text: String, cx: &mut App) {
    let previous = cx.global_mut::<AppSettings>().write_chain.take();
    let executor = cx.background_executor().clone();
    let task = cx.spawn(async move |cx| {
        if let Some(previous) = previous {
            previous.await;
        }
        let write = executor.spawn({
            let path = path.clone();
            async move { atomic_write::write(&path, text.as_bytes()) }
        });
        if let Err(error) = write.await {
            tracing::error!(
                %error,
                path = %path.display(),
                "failed to write the global settings"
            );
            cx.update(|cx| report_write_failure(path, error.to_string(), cx));
        }
    });
    cx.global_mut::<AppSettings>().write_chain = Some(task);
}

/// Surface a settings write failure to the user through the project
/// notification channel.
fn report_write_failure(path: PathBuf, error: String, cx: &mut App) {
    let Some(project) = cx
        .try_global::<ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        // No session to show it in (shutdown, or a headless tool): the log
        // above is the whole report.
        return;
    };
    project.update(cx, |project, cx| {
        project.report_settings_write_failure(path, error, cx);
    });
}

/// Mark the open project as having unsaved changes after a project-layer
/// edit. Its `settings.toml` entry now differs from the file on disk, which is
/// exactly what the unsaved-changes guard and the title bar report.
fn mark_project_dirty(cx: &mut App) {
    let Some(project) = cx
        .try_global::<ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        return;
    };
    project.update(cx, |project, cx| {
        project.mark_settings_changed(cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectFile;
    use crate::project::settings::{AutoSaveLayer, PlaybackLayer};
    use crate::project_state::{ProjectEvent, ProjectState, disable_background_eval_for_tests};
    use gpui::{AppContext as _, TestAppContext};

    fn write_toml(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_missing_global_settings_file_resolves_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = read_global_settings_at(Some(dir.path().join("absent.toml")));
        assert_eq!(file.resolved(), ResolvedSettings::default());
        // No config directory at all is the same outcome, not a panic.
        assert_eq!(
            read_global_settings_at(None).resolved(),
            ResolvedSettings::default()
        );
    }

    /// A hand-edited file with a syntax error must not stop a launch, and must
    /// not take the readable settings of *other* layers with it.
    #[test]
    fn a_corrupt_global_settings_file_resolves_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        for broken in ["[playback\nframe_rate = ", "locale = ", "\u{0}\u{1}"] {
            write_toml(&path, broken);
            let file = read_global_settings_at(Some(path.clone()));
            assert_eq!(file.resolved(), ResolvedSettings::default(), "{broken:?}");
        }
    }

    #[test]
    fn a_readable_global_settings_file_supplies_its_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        write_toml(
            &path,
            "locale = \"ja\"\n\n[playback]\nframe_rate = \"24\"\n",
        );
        let resolved = read_global_settings_at(Some(path)).resolved();
        assert_eq!(resolved.locale, "ja");
        assert_eq!(resolved.frame_rate, "24");
        // Untouched fields still come from the built-in defaults.
        assert_eq!(
            resolved.auto_save_interval_seconds,
            ResolvedSettings::default().auto_save_interval_seconds
        );
    }

    /// The runtime resolution must be the same function the project file
    /// helper performs, so wiring settings into the app cannot quietly change
    /// what a layered value resolves to.
    #[gpui::test]
    fn the_runtime_resolution_matches_the_project_file_helper(cx: &mut TestAppContext) {
        let global = SettingsLayer {
            locale: Some("ja".into()),
            playback: PlaybackLayer {
                frame_rate: Some("24".into()),
                proxy_resolution: Some(1.0),
                ..Default::default()
            },
            auto_save: AutoSaveLayer {
                enabled: Some(false),
                interval_seconds: Some(600),
            },
            ..Default::default()
        };
        let mut project = ProjectFile::new("p", "2026-01-01T00:00:00Z");
        project.settings.playback.proxy_resolution = Some(0.25);
        project.settings.auto_save.interval_seconds = Some(30);

        let runtime = cx.update(|cx| {
            install(
                GlobalSettingsFile {
                    layer: global.clone(),
                    path: None,
                },
                cx,
            );
            set_project_layer(project.settings.clone(), cx);
            resolved(cx)
        });

        assert_eq!(runtime, project.resolved_settings(Some(&global), None));
        // Spot-check the direction the whole layer model exists for.
        assert_eq!(runtime.proxy_resolution, 0.25, "the project layer wins");
        assert_eq!(runtime.frame_rate, "24", "the global layer still applies");
    }

    /// The completion criterion for layer independence: writing one layer
    /// leaves the other layer's file byte-identical.
    #[gpui::test]
    fn a_global_edit_writes_only_the_global_layer_file(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let dir = tempfile::tempdir().unwrap();
        let global_path = dir.path().join("config").join("settings.toml");
        let project_path = dir.path().join("demo.ravprj");
        let mut project = ProjectFile::new("demo", "2026-01-01T00:00:00Z");
        project.settings.locale = Some("en".into());
        project.save(&project_path).unwrap();
        let project_bytes = std::fs::read(&project_path).unwrap();

        cx.update(|cx| {
            install(read_global_settings_at(Some(global_path.clone())), cx);
            update(SettingsScope::Global, |l| l.locale = Some("ja".into()), cx);
        });
        cx.run_until_parked();

        let written = SettingsLayer::from_toml(&std::fs::read_to_string(&global_path).unwrap())
            .expect("the written global layer parses");
        assert_eq!(written.locale.as_deref(), Some("ja"));
        assert_eq!(
            std::fs::read(&project_path).unwrap(),
            project_bytes,
            "the project archive is untouched by a global edit"
        );

        // Removing the override deletes it from the file rather than writing
        // the default as an explicit value ("reset to default").
        cx.update(|cx| update(SettingsScope::Global, |l| l.locale = None, cx));
        cx.run_until_parked();
        let written = SettingsLayer::from_toml(&std::fs::read_to_string(&global_path).unwrap())
            .expect("the written global layer parses");
        assert_eq!(written.locale, None);
        assert_eq!(cx.update(|cx| resolved(cx)).locale, DEFAULT_LOCALE);
    }

    /// The other direction: a project-layer edit writes no global file at all.
    /// It travels with the document, so it also has to leave the project
    /// dirty.
    #[gpui::test]
    fn a_project_edit_leaves_the_global_layer_file_untouched(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let dir = tempfile::tempdir().unwrap();
        let global_path = dir.path().join("settings.toml");
        write_toml(&global_path, "locale = \"en\"\n");
        let before = std::fs::read(&global_path).unwrap();

        let project = cx.new(ProjectState::new);
        cx.update(|cx| {
            cx.set_global(crate::project_state::ProjectStateHandle(
                project.downgrade(),
            ));
            install(read_global_settings_at(Some(global_path.clone())), cx);
            update(
                SettingsScope::Project,
                |l| l.playback.frame_rate = Some("24".into()),
                cx,
            );
        });
        cx.run_until_parked();

        assert_eq!(cx.update(|cx| resolved(cx)).frame_rate, "24");
        assert_eq!(
            std::fs::read(&global_path).unwrap(),
            before,
            "the global settings file is untouched by a project edit"
        );
        assert!(
            project.read_with(cx, |project, _| project.is_dirty()),
            "an unsaved project-layer edit is an unsaved change"
        );
    }

    /// The completion criterion for silent loss: a write that cannot happen
    /// reaches the user as a notification event, not just a log line.
    #[gpui::test]
    fn a_failed_global_write_is_reported_as_an_event(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the settings directory would be: creating the
        // parent fails on every platform, which makes the failure injectable
        // without depending on directory permissions.
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"file").unwrap();
        let global_path = blocker.join("ravel").join("settings.toml");

        let project = cx.new(ProjectState::new);
        let failures = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = failures.clone();
        cx.update(|cx| {
            cx.set_global(crate::project_state::ProjectStateHandle(
                project.downgrade(),
            ));
            cx.subscribe(&project, move |_project, event: &ProjectEvent, _cx| {
                if let ProjectEvent::SettingsSaveFailed { path, .. } = event {
                    recorded.lock().unwrap().push(path.clone());
                }
            })
            .detach();
            install(read_global_settings_at(Some(global_path.clone())), cx);
            update(SettingsScope::Global, |l| l.locale = Some("ja".into()), cx);
        });
        cx.run_until_parked();

        assert_eq!(
            failures.lock().unwrap().as_slice(),
            std::slice::from_ref(&global_path),
            "the failed settings write is reported once, with its path"
        );
        // The edit still took effect in memory: the user sees the change and
        // the warning, rather than a control that silently springs back.
        assert_eq!(cx.update(|cx| resolved(cx)).locale, "ja");
    }

    /// Without an installed global (a panel built outside the bootstrap) the
    /// readers answer with defaults and an edit is a logged no-op.
    #[gpui::test]
    fn reads_and_writes_are_inert_without_the_global(cx: &mut TestAppContext) {
        cx.update(|cx| {
            assert_eq!(resolved(cx), ResolvedSettings::default());
            assert_eq!(layer(SettingsScope::Global, cx), SettingsLayer::default());
            update(SettingsScope::Global, |l| l.locale = Some("ja".into()), cx);
            assert_eq!(resolved(cx), ResolvedSettings::default());
        });
    }
}

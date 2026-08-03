// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The settings screens: Preferences and Project Settings (REQ-PROJ-004).
//!
//! Two dialogs, not two pages of one dialog. **The screen decides the settings
//! layer it writes**, so no row ever has to choose one: Preferences writes the
//! `global` layer and Project Settings the `project` layer, which is the order
//! `default → global → project → user` resolves in — a project override that
//! loses to a preference would be the wrong way round. The `user` layer is
//! reserved for a machine-local final override and is never written from here.
//!
//! Both are modal dialogs rather than dockable panels (a panel would drag every
//! workspace preset asset along) and rather than separate windows (which would
//! depend on the detached-window debt, `MED-APP-01`). Changes apply as they are
//! made, so the footer only closes the dialog: there is nothing to confirm and
//! nothing to cancel, and settings are not document edits, so they stay off the
//! undo stack.
//!
//! This unit builds the shell only: the page model and the sidebar exist, but
//! no page carries a field yet. Fields arrive with the features that make them
//! do something — `SET-3` (theme mode and theme), `SET-4` (language), `SET-5`
//! (keybinding list), `SET-6` (default frame rate) — because a setting that
//! changes nothing must not be on screen
//! (`docs/implementation/settings-screen-plan.md`).
//!
//! **Every field added later binds `SettingField::on_reset(is_dirty, reset)`
//! and never `SettingField::default_value()`.** `default_value` writes the
//! default back as an explicit value, which in a layered model *creates* an
//! override instead of dropping one; `is_dirty` means "this layer holds a
//! value" and `reset` means "remove it from this layer".

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_component::setting::{SettingGroup, SettingPage, Settings};
use ravel_i18n::t;

/// Height of the dialog body. The settings component fills its container
/// (sidebar and page list both scroll inside it), so the dialog has to give it
/// a bounded height rather than let the content decide one.
const BODY_HEIGHT: Pixels = px(420.0);

/// Width of the page sidebar.
const SIDEBAR_WIDTH: Pixels = px(176.0);

/// Which settings screen a dialog shows — and therefore which settings layer
/// its fields write to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsScope {
    /// Preferences: writes the `global` layer (user-wide, survives projects).
    Preferences,
    /// Project settings: writes the `project` layer (inside the `.ravprj`).
    Project,
}

impl SettingsScope {
    /// i18n key for the dialog title.
    pub fn title_key(self) -> &'static str {
        match self {
            Self::Preferences => "settings.dialog.preferences_title",
            Self::Project => "settings.dialog.project_title",
        }
    }

    /// Element id of the settings component, which keys the window state
    /// holding the selected page and the search query. The two screens must not
    /// share it, or opening one would land on the other's page.
    fn element_id(self) -> &'static str {
        match self {
            Self::Preferences => "settings-preferences",
            Self::Project => "settings-project",
        }
    }

    /// The sidebar pages of this screen, in order.
    pub fn pages(self) -> &'static [SettingsPageKind] {
        match self {
            Self::Preferences => &[
                SettingsPageKind::Appearance,
                SettingsPageKind::Language,
                SettingsPageKind::Keybindings,
            ],
            Self::Project => &[SettingsPageKind::Project],
        }
    }
}

/// A page in a settings screen's sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPageKind {
    /// Theme mode and theme selection (`SET-3`).
    Appearance,
    /// UI language (`SET-4`).
    Language,
    /// Keybinding list (`SET-5`).
    Keybindings,
    /// Project-level settings (`SET-6`).
    Project,
}

impl SettingsPageKind {
    /// Every page, for the locale-coverage test.
    pub const ALL: [Self; 4] = [
        Self::Appearance,
        Self::Language,
        Self::Keybindings,
        Self::Project,
    ];

    /// i18n key for the page's sidebar entry and header.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Appearance => "settings.page.appearance",
            Self::Language => "settings.page.language",
            Self::Keybindings => "settings.page.keybindings",
            Self::Project => "settings.page.project",
        }
    }
}

/// A page and the groups it shows.
///
/// The seam later units extend: a unit that makes a setting take effect returns
/// its group from [`groups_for`], and the page it belongs to picks it up.
struct PageSpec {
    kind: SettingsPageKind,
    groups: Vec<SettingGroup>,
}

fn page_specs(scope: SettingsScope) -> Vec<PageSpec> {
    scope
        .pages()
        .iter()
        .map(|kind| PageSpec {
            kind: *kind,
            groups: groups_for(*kind),
        })
        .collect()
}

/// The groups a page shows, empty while the page has nothing that works yet.
///
/// Exhaustive on purpose: a new page cannot be added without deciding what it
/// shows.
fn groups_for(kind: SettingsPageKind) -> Vec<SettingGroup> {
    match kind {
        SettingsPageKind::Appearance
        | SettingsPageKind::Language
        | SettingsPageKind::Keybindings
        | SettingsPageKind::Project => Vec::new(),
    }
}

/// The body of a settings dialog.
pub struct SettingsDialog {
    scope: SettingsScope,
    focus_handle: FocusHandle,
}

impl SettingsDialog {
    pub fn new(scope: SettingsScope, cx: &mut Context<Self>) -> Self {
        // The dialog owns the focus trap. A view must not grab focus while it
        // is being constructed (`.agents/rules/gpui.md`), and this one is not a
        // panel, so it never writes `FocusedPanelGlobal` either: the workspace
        // keeps the focused panel it had, and `open_dialog` hands the focus
        // back to it on close.
        Self {
            scope,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Which screen this dialog is (exposed for tests).
    pub fn scope(&self) -> SettingsScope {
        self.scope
    }
}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let specs = page_specs(self.scope);
        // `Settings` drops a page whose groups hold no item (its search filter
        // is what builds the sidebar), so while no field exists the sidebar and
        // the page body are both empty. Say that instead of showing a blank
        // box; the note disappears by itself once `groups_for` returns a group.
        let awaiting_fields = specs.iter().all(|spec| spec.groups.is_empty());
        let pages = specs.into_iter().map(|spec| {
            SettingPage::new(SharedString::from(t!(spec.kind.label_key()))).groups(spec.groups)
        });

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .w_full()
            .h(BODY_HEIGHT)
            .child(
                div().flex_1().min_h(px(0.0)).child(
                    Settings::new(self.scope.element_id())
                        .sidebar_width(SIDEBAR_WIDTH)
                        .pages(pages),
                ),
            )
            .when(awaiting_fields, |this| {
                this.child(
                    div()
                        .w_full()
                        .pt_2()
                        .text_xs()
                        .text_color(cx.theme().colors.muted_foreground)
                        .child(SharedString::from(t!("settings.dialog.empty"))),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back to
    // the built-in one so `#[test]` resolves to the real one.
    use core::prelude::v1::test;

    fn catalog(locale: &str) -> toml::Table {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/locales")
            .join(format!("{locale}.toml"));
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            .parse::<toml::Table>()
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    fn has_key(table: &toml::Table, dotted_key: &str) -> bool {
        let mut current = toml::Value::Table(table.clone());
        for segment in dotted_key.split('.') {
            match current.as_table().and_then(|t| t.get(segment)) {
                Some(value) => current = value.clone(),
                None => return false,
            }
        }
        true
    }

    /// Every string the dialogs render exists in **every** locale, not just the
    /// English fallback: the `ravel-ui` coverage tests only walk `en.toml`, so a
    /// missing Japanese entry would otherwise show English silently.
    #[test]
    fn every_locale_carries_the_settings_dialog_keys() {
        let keys: Vec<&'static str> = [
            SettingsScope::Preferences.title_key(),
            SettingsScope::Project.title_key(),
            "settings.dialog.empty",
            "ui.close",
        ]
        .into_iter()
        .chain(SettingsPageKind::ALL.iter().map(|page| page.label_key()))
        .collect();

        for locale in ["en", "ja"] {
            let catalog = catalog(locale);
            for key in &keys {
                assert!(
                    has_key(&catalog, key),
                    "{locale}.toml is missing the settings dialog key \"{key}\""
                );
            }
        }
    }

    /// The pages of the two screens partition the page set: a page that belongs
    /// to no screen is unreachable, and one that belongs to both would key its
    /// state twice.
    #[test]
    fn each_page_belongs_to_exactly_one_screen() {
        for page in SettingsPageKind::ALL {
            let screens = [SettingsScope::Preferences, SettingsScope::Project]
                .into_iter()
                .filter(|scope| scope.pages().contains(&page))
                .count();
            assert_eq!(screens, 1, "{page:?} must appear on exactly one screen");
        }
    }
}

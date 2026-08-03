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
//! A page carries a field only once the setting behind it takes effect, because
//! a setting that changes nothing must not be on screen
//! (`docs/implementation/settings-screen-plan.md`). Appearance (`SET-3`) and
//! Language (`SET-4`) are here; Keybindings (`SET-5`) and Project (`SET-6`) are
//! still empty, and `Settings` drops a page with no item, so those two do not
//! appear in the sidebar yet.
//!
//! **Every field binds `SettingField::on_reset(is_dirty, reset)` and never
//! `SettingField::default_value()`.** `default_value` writes the default back as
//! an explicit value, which in a layered model *creates* an override instead of
//! dropping one; `is_dirty` means "this layer holds a value" and `reset` means
//! "remove it from this layer".
//!
//! A field is a pair of closures over the settings global — it reads the value
//! in force and writes one layer, and nothing else. In particular no field
//! touches the subsystem it configures: the write goes to
//! [`crate::app_settings::update`], which is the single place a resolved value
//! reaches `ravel_i18n` or the `Theme`. So the labels here are produced fresh on
//! every render (from `t!`), which is also how the dialog's own text follows a
//! language change made inside it.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use ravel_i18n::t;

use crate::app_settings::{self, SettingsScope as SettingsLayerScope};
use crate::project::settings::AppearanceMode;

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

    /// Debug selector of the dialog body, so a test can tell *which* screen a
    /// command opened rather than only that some dialog is up.
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::Preferences => "settings-dialog-preferences",
            Self::Project => "settings-dialog-project",
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

fn page_specs(scope: SettingsScope, cx: &App) -> Vec<PageSpec> {
    scope
        .pages()
        .iter()
        .map(|kind| PageSpec {
            kind: *kind,
            groups: groups_for(*kind, cx),
        })
        .collect()
}

/// The groups a page shows, empty while the page has nothing that works yet.
///
/// Exhaustive on purpose: a new page cannot be added without deciding what it
/// shows.
fn groups_for(kind: SettingsPageKind, cx: &App) -> Vec<SettingGroup> {
    match kind {
        SettingsPageKind::Appearance => vec![appearance_group(cx)],
        SettingsPageKind::Language => vec![language_group()],
        // `SET-5` (keybinding list) and `SET-6` (default frame rate).
        SettingsPageKind::Keybindings | SettingsPageKind::Project => Vec::new(),
    }
}

// ===========================================================================
// Appearance (`SET-3`)
// ===========================================================================

const APPEARANCE_GROUP: &str = "settings.appearance.group";
const THEME_MODE: &str = "settings.appearance.mode";
const THEME_MODE_DESCRIPTION: &str = "settings.appearance.mode_description";
const LIGHT_THEME: &str = "settings.appearance.light_theme";
const LIGHT_THEME_DESCRIPTION: &str = "settings.appearance.light_theme_description";
const DARK_THEME: &str = "settings.appearance.dark_theme";
const DARK_THEME_DESCRIPTION: &str = "settings.appearance.dark_theme_description";

/// Theme mode, and the theme worn in each mode.
fn appearance_group(cx: &App) -> SettingGroup {
    SettingGroup::new()
        .title(t!(APPEARANCE_GROUP))
        .item(theme_mode_item())
        .item(theme_item(ThemeMode::Light, cx))
        .item(theme_item(ThemeMode::Dark, cx))
}

/// System / Light / Dark.
fn theme_mode_item() -> SettingItem {
    let options = AppearanceMode::ALL
        .into_iter()
        .map(|mode| {
            (
                SharedString::from(mode.as_str()),
                SharedString::from(t!(mode_label_key(mode))),
            )
        })
        .collect();
    SettingItem::new(
        t!(THEME_MODE),
        SettingField::dropdown(
            options,
            |cx| SharedString::from(app_settings::resolved(cx).theme_mode.as_str()),
            |value, cx| {
                let Some(mode) = AppearanceMode::from_value(&value) else {
                    // The option ids come from `AppearanceMode::ALL`, so this is
                    // unreachable unless the two drift apart; refusing beats
                    // writing a value the settings file cannot express.
                    tracing::warn!(%value, "ignoring an unknown theme mode");
                    return;
                };
                app_settings::update(
                    SettingsLayerScope::Global,
                    |layer| layer.appearance.theme_mode = Some(mode),
                    cx,
                );
            },
        )
        .on_reset(
            |cx| {
                app_settings::layer(SettingsLayerScope::Global, cx)
                    .appearance
                    .theme_mode
                    .is_some()
            },
            |_window, cx| {
                app_settings::update(
                    SettingsLayerScope::Global,
                    |layer| layer.appearance.theme_mode = None,
                    cx,
                );
            },
        ),
    )
    .description(t!(THEME_MODE_DESCRIPTION))
}

/// The theme worn in one mode.
///
/// The options are the registry's themes **of that mode**: the light slot
/// offering a dark theme would be a way to make the UI unreadable by picking the
/// wrong row. A `scrollable_dropdown` because the list grows with every file the
/// user drops in `assets/themes` and would otherwise run past the viewport.
///
/// The value shown is the theme actually in force rather than the name the
/// settings hold, so a name no theme carries any more shows the theme the user is
/// looking at (the fallback in
/// [`app_settings::apply_resolved_appearance`]) instead of a phantom selection.
/// The settings keep the requested name regardless, and the reset control is
/// driven by the layer, not by this value.
fn theme_item(mode: ThemeMode, cx: &App) -> SettingItem {
    let options = ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|config| config.mode == mode)
        .map(|config| (config.name.clone(), config.name.clone()))
        .collect();
    let (title, description) = match mode {
        ThemeMode::Light => (LIGHT_THEME, LIGHT_THEME_DESCRIPTION),
        ThemeMode::Dark => (DARK_THEME, DARK_THEME_DESCRIPTION),
    };
    SettingItem::new(
        t!(title),
        SettingField::scrollable_dropdown(
            options,
            move |cx| theme_in_force(mode, cx),
            move |value, cx| {
                let name = value.to_string();
                app_settings::update(
                    SettingsLayerScope::Global,
                    move |layer| match mode {
                        ThemeMode::Light => layer.appearance.light_theme = Some(name),
                        ThemeMode::Dark => layer.appearance.dark_theme = Some(name),
                    },
                    cx,
                );
            },
        )
        .on_reset(
            move |cx| {
                let appearance = app_settings::layer(SettingsLayerScope::Global, cx).appearance;
                match mode {
                    ThemeMode::Light => appearance.light_theme.is_some(),
                    ThemeMode::Dark => appearance.dark_theme.is_some(),
                }
            },
            move |_window, cx| {
                app_settings::update(
                    SettingsLayerScope::Global,
                    move |layer| match mode {
                        ThemeMode::Light => layer.appearance.light_theme = None,
                        ThemeMode::Dark => layer.appearance.dark_theme = None,
                    },
                    cx,
                );
            },
        ),
    )
    .description(t!(description))
}

/// The name of the theme `mode` is currently wearing.
fn theme_in_force(mode: ThemeMode, cx: &App) -> SharedString {
    let Some(theme) = cx.try_global::<Theme>() else {
        // No theme global (a headless tool): name what the settings ask for.
        let settings = app_settings::resolved(cx);
        return SharedString::from(match mode {
            ThemeMode::Light => settings.light_theme,
            ThemeMode::Dark => settings.dark_theme,
        });
    };
    match mode {
        ThemeMode::Light => theme.light_theme.name.clone(),
        ThemeMode::Dark => theme.dark_theme.name.clone(),
    }
}

/// i18n key for a theme mode's dropdown option.
fn mode_label_key(mode: AppearanceMode) -> &'static str {
    match mode {
        AppearanceMode::System => "settings.appearance.mode_system",
        AppearanceMode::Light => "settings.appearance.mode_light",
        AppearanceMode::Dark => "settings.appearance.mode_dark",
    }
}

// ===========================================================================
// Language (`SET-4`)
// ===========================================================================

const LANGUAGE_GROUP: &str = "settings.language.group";
const UI_LANGUAGE: &str = "settings.language.ui";
const UI_LANGUAGE_DESCRIPTION: &str = "settings.language.ui_description";

/// The interface language.
fn language_group() -> SettingGroup {
    SettingGroup::new().title(t!(LANGUAGE_GROUP)).item(
        SettingItem::new(
            t!(UI_LANGUAGE),
            SettingField::dropdown(
                locale_options(),
                |cx| SharedString::from(app_settings::resolved(cx).locale),
                |value, cx| {
                    app_settings::update(
                        SettingsLayerScope::Global,
                        |layer| layer.locale = Some(value.to_string()),
                        cx,
                    );
                },
            )
            .on_reset(
                |cx| {
                    app_settings::layer(SettingsLayerScope::Global, cx)
                        .locale
                        .is_some()
                },
                |_window, cx| {
                    app_settings::update(
                        SettingsLayerScope::Global,
                        |layer| layer.locale = None,
                        cx,
                    );
                },
            ),
        )
        .description(t!(UI_LANGUAGE_DESCRIPTION)),
    )
}

/// The locales the catalogs offer, each labelled in its own language.
///
/// Sorted by code: [`ravel_i18n::available_locales`] answers from a `HashMap`, and
/// a list that comes out in a different order every time the dialog opens is not
/// a list anyone can use. Sorting by *label* would reorder itself as languages
/// are added and would depend on collation; the code is stable and is what the
/// settings file records.
///
/// A locale whose catalog does not name itself is offered under its bare code
/// rather than dropped — an unlabelled language the user can still reach beats a
/// language that has silently disappeared.
fn locale_options() -> Vec<(SharedString, SharedString)> {
    let mut codes = ravel_i18n::available_locales();
    codes.sort();
    codes
        .into_iter()
        .map(|code| {
            let label = ravel_i18n::locale_display_name(&code).unwrap_or_else(|| code.clone());
            (SharedString::from(code), SharedString::from(label))
        })
        .collect()
}

/// Every i18n key the fields of `kind` render.
///
/// Exposed so the locale-coverage test can walk them, and so the language switch
/// has something to assert against: these are the strings the dialog produces on
/// each render, so if they follow the active locale, so does the dialog.
pub fn label_keys(kind: SettingsPageKind) -> Vec<&'static str> {
    match kind {
        SettingsPageKind::Appearance => vec![
            APPEARANCE_GROUP,
            THEME_MODE,
            THEME_MODE_DESCRIPTION,
            mode_label_key(AppearanceMode::System),
            mode_label_key(AppearanceMode::Light),
            mode_label_key(AppearanceMode::Dark),
            LIGHT_THEME,
            LIGHT_THEME_DESCRIPTION,
            DARK_THEME,
            DARK_THEME_DESCRIPTION,
        ],
        SettingsPageKind::Language => {
            vec![LANGUAGE_GROUP, UI_LANGUAGE, UI_LANGUAGE_DESCRIPTION]
        }
        SettingsPageKind::Keybindings | SettingsPageKind::Project => Vec::new(),
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

    /// Which screen this dialog shows, and therefore which settings layer the
    /// fields on it write to.
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
        let specs = page_specs(self.scope, cx);
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
            .debug_selector(|| self.scope.debug_selector().to_string())
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
        .chain(
            SettingsPageKind::ALL
                .iter()
                .flat_map(|page| label_keys(*page)),
        )
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

    /// Every locale names itself, which is what the language picker labels its
    /// options with. A catalog without the key would appear as a bare locale
    /// code in the one dialog whose whole job is to be readable to someone who
    /// cannot read the current language.
    #[test]
    fn every_locale_names_itself() {
        for locale in ["en", "ja"] {
            assert!(
                has_key(&catalog(locale), "language.name"),
                "{locale}.toml must name itself in `language.name`"
            );
        }
    }

    /// A page shows fields exactly when the settings behind them take effect:
    /// Appearance and Language do, Keybindings and Project do not yet. Pinning
    /// this keeps "what is on screen works" from decaying into a screen full of
    /// dead controls.
    #[test]
    fn only_the_pages_whose_settings_apply_carry_labels() {
        assert!(!label_keys(SettingsPageKind::Appearance).is_empty());
        assert!(!label_keys(SettingsPageKind::Language).is_empty());
        assert!(label_keys(SettingsPageKind::Keybindings).is_empty());
        assert!(label_keys(SettingsPageKind::Project).is_empty());
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

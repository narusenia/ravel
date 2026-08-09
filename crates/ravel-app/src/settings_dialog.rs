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
//! (`docs/implementation/settings-screen-plan.md`). General (`SET-16`),
//! Appearance (`SET-3`), Language (`SET-4`), the read-only Keybindings list
//! (`SET-5`) and the project's default frame rate (`SET-6`) are what applies
//! today; cache, auto save, proxy and colour settings are absent until the
//! features behind them exist (`SET-8`–`SET-11`). `Settings` drops a page with
//! no item, so a page added ahead of its fields would simply not appear in the
//! sidebar.
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

use gpui::*;
use gpui_component::setting::{
    AnySettingField, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use ravel_i18n::t;
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;

use crate::keybindings::{KeybindingRow, current_row};

use crate::app_settings::{self, SettingsScope as SettingsLayerScope};
use ravel_project::settings::AppearanceMode;

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
                SettingsPageKind::General,
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
    /// Behaviour that belongs to no other page: playback and startup
    /// (`SET-16`).
    General,
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
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::Appearance,
        Self::Language,
        Self::Keybindings,
        Self::Project,
    ];

    /// i18n key for the page's sidebar entry and header.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::General => "settings.page.general",
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

/// The groups a page shows — empty only for a page whose settings do not apply
/// yet, which `Settings` then drops from the sidebar.
///
/// Two kinds of page meet here. Most are a list of settings the user changes,
/// so they are built from [`fields_for`] — one [`SettingItem`] per [`PageField`],
/// which keeps the field reachable for the reset wiring. The Keybindings page is
/// not that: it reports assignments it cannot change (editing is `SET-12`), so
/// it builds its own group and has no fields.
fn groups_for(kind: SettingsPageKind, cx: &App) -> Vec<SettingGroup> {
    if kind == SettingsPageKind::Keybindings {
        return vec![keybinding_group()];
    }
    let Some(group_key) = group_key(kind) else {
        return Vec::new();
    };
    let items = fields_for(kind, cx).into_iter().map(PageField::into_item);
    vec![SettingGroup::new().title(t!(group_key)).items(items)]
}

/// i18n key for a page's single group of *fields*, or `None` for a page that has
/// none — either because its settings do not apply yet, or because the page is
/// not built from fields at all (Keybindings; see [`groups_for`]).
///
/// Exhaustive on purpose: a new page cannot be added without deciding what it
/// shows.
fn group_key(kind: SettingsPageKind) -> Option<&'static str> {
    match kind {
        SettingsPageKind::General => Some(GENERAL_GROUP),
        SettingsPageKind::Appearance => Some(APPEARANCE_GROUP),
        SettingsPageKind::Language => Some(LANGUAGE_GROUP),
        SettingsPageKind::Project => Some(PROJECT_GROUP),
        // The keybinding list is not a field list.
        SettingsPageKind::Keybindings => None,
    }
}

/// One row of a settings page: what it is called, and the value it reads and
/// writes.
///
/// The field is handed out whole rather than pre-wrapped in a [`SettingItem`]
/// because a `SettingItem` closes over its contents — nothing can be asked of it
/// afterwards — while the field still answers
/// [`AnySettingField::is_resettable`](gpui_component::setting::AnySettingField)
/// and can be [`reset`](gpui_component::setting::AnySettingField::reset). That is
/// the reset wiring the plan bans `default_value` in favour of, so it needs to be
/// reachable by something other than a mouse.
pub struct PageField {
    /// i18n key for the row's label.
    pub title_key: &'static str,
    /// i18n key for the sentence under it.
    pub description_key: &'static str,
    /// The control, bound to the settings layer this screen writes.
    pub field: FieldControl,
}

/// A row's control, by the type of value it carries.
///
/// `SettingField` is generic over that type and `SettingItem::new` is generic
/// over the field, so a page holding rows of both kinds needs one name for them
/// — a boolean switch and a string dropdown are not the same type and never
/// will be. Two variants because two are what the dialog uses; a number row
/// (`SettingField<f64>`) adds a third when a setting needs one.
pub enum FieldControl {
    /// A dropdown or text field over a string-valued setting.
    Text(SettingField<SharedString>),
    /// A switch over a boolean setting.
    Toggle(SettingField<bool>),
}

impl FieldControl {
    /// The control as the interface the reset wiring is read through
    /// (`is_resettable` / `reset`), which is the same for either value type.
    pub fn any(&self) -> &dyn AnySettingField {
        match self {
            Self::Text(field) => field,
            Self::Toggle(field) => field,
        }
    }
}

impl PageField {
    /// The row as the dialog renders it. Consumes the field, which is why
    /// [`fields_for`] hands out the parts rather than the finished item.
    fn into_item(self) -> SettingItem {
        let (title, description) = (t!(self.title_key), t!(self.description_key));
        match self.field {
            FieldControl::Text(field) => SettingItem::new(title, field).description(description),
            FieldControl::Toggle(field) => SettingItem::new(title, field).description(description),
        }
    }
}

/// The rows a page shows, in order.
pub fn fields_for(kind: SettingsPageKind, cx: &App) -> Vec<PageField> {
    match kind {
        SettingsPageKind::General => vec![
            stop_returns_to_play_start_field(),
            startup_create_composition_field(),
        ],
        SettingsPageKind::Appearance => vec![
            theme_mode_field(),
            theme_field(ThemeMode::Light, cx),
            theme_field(ThemeMode::Dark, cx),
        ],
        SettingsPageKind::Language => vec![language_field()],
        SettingsPageKind::Project => vec![default_frame_rate_field(cx)],
        SettingsPageKind::Keybindings => Vec::new(),
    }
}

// ===========================================================================
// General (`SET-16`)
// ===========================================================================

const GENERAL_GROUP: &str = "settings.general.group";
const STOP_RETURNS_TO_PLAY_START: &str = "settings.general.stop_returns_to_play_start";
const STOP_RETURNS_TO_PLAY_START_DESCRIPTION: &str =
    "settings.general.stop_returns_to_play_start_description";
const STARTUP_CREATE_COMPOSITION: &str = "settings.general.startup_create_composition";
const STARTUP_CREATE_COMPOSITION_DESCRIPTION: &str =
    "settings.general.startup_create_composition_description";

/// Write the Stop landing point into the preferences layer.
///
/// A named function rather than a closure body because `SettingField`'s setter
/// is `pub(crate)` to gpui-component: a test cannot reach the switch's own
/// closure, so the closure is a one-line delegation to something a test *can*
/// call, and "which layer does this row write" stops being untestable.
pub fn set_stop_returns_to_play_start(value: bool, cx: &mut App) {
    app_settings::update(
        SettingsLayerScope::Global,
        |layer| layer.playback.stop_returns_to_play_start = Some(value),
        cx,
    );
}

/// [`set_stop_returns_to_play_start`] for the startup composition switch.
pub fn set_startup_create_composition(value: bool, cx: &mut App) {
    app_settings::update(
        SettingsLayerScope::Global,
        |layer| layer.startup.create_composition = Some(value),
        cx,
    );
}

/// Where Stop leaves the playhead.
///
/// A switch rather than a two-option dropdown: the setting is a boolean in the
/// file and reads as one on screen, and a dropdown would ask the user to read
/// two labels to find out which one is "off".
fn stop_returns_to_play_start_field() -> PageField {
    PageField {
        title_key: STOP_RETURNS_TO_PLAY_START,
        description_key: STOP_RETURNS_TO_PLAY_START_DESCRIPTION,
        field: FieldControl::Toggle(
            SettingField::switch(
                |cx| app_settings::resolved(cx).stop_returns_to_play_start,
                set_stop_returns_to_play_start,
            )
            .on_reset(
                |cx| {
                    app_settings::layer(SettingsLayerScope::Global, cx)
                        .playback
                        .stop_returns_to_play_start
                        .is_some()
                },
                |_window, cx| {
                    app_settings::update(
                        SettingsLayerScope::Global,
                        |layer| layer.playback.stop_returns_to_play_start = None,
                        cx,
                    );
                },
            ),
        ),
    }
}

/// Whether a document with nothing to open starts on one empty composition.
///
/// A preference rather than a project setting even though it decides what a
/// document contains: it applies to the document being *built*, which has no
/// project layer to read yet (`ProjectState::fresh_document`).
fn startup_create_composition_field() -> PageField {
    PageField {
        title_key: STARTUP_CREATE_COMPOSITION,
        description_key: STARTUP_CREATE_COMPOSITION_DESCRIPTION,
        field: FieldControl::Toggle(
            SettingField::switch(
                |cx| app_settings::resolved(cx).startup_creates_composition,
                set_startup_create_composition,
            )
            .on_reset(
                |cx| {
                    app_settings::layer(SettingsLayerScope::Global, cx)
                        .startup
                        .create_composition
                        .is_some()
                },
                |_window, cx| {
                    app_settings::update(
                        SettingsLayerScope::Global,
                        |layer| layer.startup.create_composition = None,
                        cx,
                    );
                },
            ),
        ),
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

/// System / Light / Dark.
fn theme_mode_field() -> PageField {
    let options = AppearanceMode::ALL
        .into_iter()
        .map(|mode| {
            (
                SharedString::from(mode.as_str()),
                SharedString::from(t!(mode_label_key(mode))),
            )
        })
        .collect();
    PageField {
        title_key: THEME_MODE,
        description_key: THEME_MODE_DESCRIPTION,
        field: FieldControl::Text(
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
        ),
    }
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
fn theme_field(mode: ThemeMode, cx: &App) -> PageField {
    // No registry (a tool that never called `gpui_component::init`) means no
    // themes to offer, not a panic inside a render.
    let options = cx
        .try_global::<ThemeRegistry>()
        .map(|registry| {
            registry
                .sorted_themes()
                .into_iter()
                .filter(|config| config.mode == mode)
                .map(|config| (config.name.clone(), config.name.clone()))
                .collect()
        })
        .unwrap_or_default();
    let (title_key, description_key) = match mode {
        ThemeMode::Light => (LIGHT_THEME, LIGHT_THEME_DESCRIPTION),
        ThemeMode::Dark => (DARK_THEME, DARK_THEME_DESCRIPTION),
    };
    PageField {
        title_key,
        description_key,
        field: FieldControl::Text(
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
        ),
    }
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
fn language_field() -> PageField {
    PageField {
        title_key: UI_LANGUAGE,
        description_key: UI_LANGUAGE_DESCRIPTION,
        field: FieldControl::Text(
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
        ),
    }
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

// ===========================================================================
// Project (`SET-6`)
// ===========================================================================

const PROJECT_GROUP: &str = "settings.project.group";
const DEFAULT_FRAME_RATE: &str = "settings.project.frame_rate";
const DEFAULT_FRAME_RATE_DESCRIPTION: &str = "settings.project.frame_rate_description";

/// The frame rates the picker offers, ascending.
///
/// The editorial rates plus the three NTSC ones, written the way an editor says
/// them — `"29.97"` is stored as typed and read back as the exact `30000/1001`
/// ([`app_settings::parse_frame_rate`]), so the file stays legible without the
/// rate drifting.
const COMMON_FRAME_RATES: [&str; 8] = ["23.976", "24", "25", "29.97", "30", "50", "59.94", "60"];

/// The option the row shows: the setting's own text while it is readable, and
/// otherwise the option that names the rate actually in force.
///
/// Without the second half the row would display a value that is not doing
/// anything — a hand-edited `frame_rate = "24fps"` is warned about and ignored by
/// [`app_settings::default_frame_rate`], so showing it as the project's frame
/// rate states the opposite of what a new composition would be built at. The
/// fallback option is found by *parsing* the list rather than by naming one, so
/// the two cannot drift apart.
fn frame_rate_option_in_force(cx: &App) -> SharedString {
    let setting = app_settings::resolved(cx).frame_rate;
    if app_settings::parse_frame_rate(&setting).is_some() {
        return SharedString::from(setting);
    }
    let in_force = app_settings::default_frame_rate(cx);
    COMMON_FRAME_RATES
        .iter()
        .find(|option| app_settings::parse_frame_rate(option) == Some(in_force))
        .map(|option| SharedString::from(*option))
        .unwrap_or_else(|| SharedString::from(setting))
}

/// The frame rate a new composition starts at when it has nothing to inherit.
///
/// A **closed list** rather than a free-text field, and that is the whole point:
/// this value is one of the two forms
/// [`app_settings::parse_frame_rate`] reads, and a text field would let `"24fps"`
/// or an empty string into the layer — where it would be silently ignored on
/// every read while the dialog kept showing it as the project's frame rate. The
/// same argument the theme mode picker makes: offer only what the settings file
/// can express.
///
/// The list is not the whole notation, though (a rational like `"30000/1001"` is
/// legal and unlisted), so a value the file already holds is offered alongside it
/// — but **only if it parses**. A rate nothing can read is not shown as the
/// current choice, because it is not in force: the reader warned and fell back.
///
/// What the row shows is the rate **in force**, which on this screen may come
/// from the global layer; whether it offers a reset is decided by the project
/// layer alone. That difference is the feature — it is how "the project
/// overrides the preference" is visible at all (REQ-PROJ-004).
fn default_frame_rate_field(cx: &App) -> PageField {
    let mut options: Vec<(SharedString, SharedString)> = COMMON_FRAME_RATES
        .iter()
        .map(|rate| (SharedString::from(*rate), SharedString::from(*rate)))
        .collect();
    let in_force = app_settings::resolved(cx).frame_rate;
    if app_settings::parse_frame_rate(&in_force).is_some()
        && !COMMON_FRAME_RATES.contains(&in_force.as_str())
    {
        options.push((
            SharedString::from(in_force.clone()),
            SharedString::from(in_force),
        ));
    }
    PageField {
        title_key: DEFAULT_FRAME_RATE,
        description_key: DEFAULT_FRAME_RATE_DESCRIPTION,
        field: FieldControl::Text(
            SettingField::dropdown(options, frame_rate_option_in_force, |value, cx| {
                if app_settings::parse_frame_rate(&value).is_none() {
                    // Unreachable while the options come from the list above;
                    // refusing beats writing a rate the settings cannot read.
                    tracing::warn!(%value, "ignoring an unusable default frame rate");
                    return;
                }
                app_settings::update(
                    SettingsLayerScope::Project,
                    |layer| layer.playback.frame_rate = Some(value.to_string()),
                    cx,
                );
            })
            .on_reset(
                |cx| {
                    app_settings::layer(SettingsLayerScope::Project, cx)
                        .playback
                        .frame_rate
                        .is_some()
                },
                |_window, cx| {
                    app_settings::update(
                        SettingsLayerScope::Project,
                        |layer| layer.playback.frame_rate = None,
                        cx,
                    );
                },
            ),
        ),
    }
}

/// Every i18n key the fields of `kind` render.
///
/// Exposed so the locale-coverage test can walk them, and so the language switch
/// has something to assert against: these are the strings the dialog produces on
/// each render, so if they follow the active locale, so does the dialog.
pub fn label_keys(kind: SettingsPageKind) -> Vec<&'static str> {
    match kind {
        SettingsPageKind::General => vec![
            GENERAL_GROUP,
            STOP_RETURNS_TO_PLAY_START,
            STOP_RETURNS_TO_PLAY_START_DESCRIPTION,
            STARTUP_CREATE_COMPOSITION,
            STARTUP_CREATE_COMPOSITION_DESCRIPTION,
        ],
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
        SettingsPageKind::Project => vec![
            PROJECT_GROUP,
            DEFAULT_FRAME_RATE,
            DEFAULT_FRAME_RATE_DESCRIPTION,
        ],
        SettingsPageKind::Keybindings => Vec::new(),
    }
}

/// The read-only list of key assignments (`SET-5`): one row per command, each
/// showing the chord in force and whether it came from the bundled defaults or
/// from the user's `keybindings.toml`.
///
/// Read-only on purpose. Editing needs conflict detection and a chord-capture
/// field, which is `SET-12`; until then a row that looked editable would be a
/// worse answer than one that plainly is not.
///
/// No row binds `on_reset`, and that is not the omission the module doc warns
/// about: reset means "drop this layer's override", and keybindings are not a
/// layered merge of `settings.toml` — they are their own file, which the user
/// either wrote or did not. There is nothing here to drop.
fn keybinding_group() -> SettingGroup {
    SettingGroup::new()
        .title(SharedString::from(t!("settings.keybindings.group")))
        .description(SharedString::from(t!("settings.keybindings.description")))
        .items(CommandId::all().map(|command| {
            SettingItem::new(
                SharedString::from(t!(command.label_key())),
                keybinding_field(command),
            )
            // The dotted id is what the user types in the file, so it has to
            // find the row: searching "step_forward" is how someone arrives
            // here from their own `keybindings.toml`.
            .keywords([SharedString::from(command.as_str())])
        }))
}

/// One row's value side: the chord in force, and its origin.
///
/// The row is resolved per render rather than captured, so it reflects the file
/// that was loaded for *this* launch — and, once `SET-12` can edit bindings, the
/// current assignment without the page needing to know it changed.
fn keybinding_field(command: CommandId) -> SettingField<SharedString> {
    SettingField::render(move |_options, _window, cx: &mut App| {
        let row = current_row(command, cx);
        let border = cx.theme().colors.border;
        let foreground = cx.theme().colors.foreground;
        let muted = cx.theme().colors.muted_foreground;
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .children(chord_chips(&row).into_iter().map(move |text| {
                div()
                    .px_1p5()
                    .rounded_sm()
                    .border_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(foreground)
                    .child(text)
            }))
            .child(div().text_xs().text_color(muted).child(origin_label(&row)))
    })
}

/// The chords to show, each once: the global one if there is one, otherwise the
/// panel-scoped ones.
///
/// `Delete` bound in two panels is one chip, not two — the chip answers "which
/// key", and [`origin_label`] answers "where does it work".
fn chord_chips(row: &KeybindingRow) -> Vec<SharedString> {
    if let Some(chord) = row.chord {
        return vec![SharedString::from(chord.to_string())];
    }
    let mut chips: Vec<SharedString> = Vec::new();
    for panel_chord in &row.panel_chords {
        let text = SharedString::from(panel_chord.chord.to_string());
        if !chips.contains(&text) {
            chips.push(text);
        }
    }
    chips
}

/// The origin text, and for a panel-scoped binding the panels it is confined to
/// — the answer to "why does this key only do something over there".
///
/// The panel names are joined with punctuation rather than a translated
/// connective because `t!` takes no arguments; the words themselves all come
/// from locale keys.
fn origin_label(row: &KeybindingRow) -> SharedString {
    let label = t!(row.origin.label_key());
    if row.panel_chords.is_empty() || row.chord.is_some() {
        return SharedString::from(label);
    }
    let names: Vec<String> = confined_panels(row)
        .into_iter()
        .map(|panel| t!(panel.label_key()))
        .collect();
    SharedString::from(format!("{label} · {}", names.join(", ")))
}

/// The distinct panels a row's panel-scoped chords are confined to, in table
/// order.
///
/// Split out from [`origin_label`] so the part with a rule in it — which panels,
/// how many times each — is testable without a locale catalog loaded.
fn confined_panels(row: &KeybindingRow) -> Vec<PanelKind> {
    let mut panels: Vec<PanelKind> = Vec::new();
    for panel_chord in &row.panel_chords {
        if !panels.contains(&panel_chord.panel) {
            panels.push(panel_chord.panel);
        }
    }
    panels
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
        // `Settings` drops a page whose groups hold no item — its search filter
        // is what builds the sidebar — so a page has to arrive with a group to be
        // reachable at all. Every page does now
        // (`every_page_carries_exactly_one_group`), which is why there is no
        // empty state here; a page added ahead of the feature behind it would
        // simply not appear, which is the right answer for a setting that does
        // nothing.
        let pages = page_specs(self.scope, cx).into_iter().map(|spec| {
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

    /// [`label_keys`] is the list the coverage tests walk, and [`fields_for`] is
    /// what the dialog actually renders: a key that only one of them knows about
    /// is either an untested label or a label that no longer exists.
    #[gpui::test]
    fn the_label_key_list_covers_every_field_a_page_renders(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for page in SettingsPageKind::ALL {
                let declared = label_keys(page);
                if let Some(group) = group_key(page) {
                    assert!(
                        declared.contains(&group),
                        "{page:?}'s group key is not in label_keys"
                    );
                }
                for field in fields_for(page, cx) {
                    for key in [field.title_key, field.description_key] {
                        assert!(
                            declared.contains(&key),
                            "{page:?} renders \"{key}\", which label_keys omits"
                        );
                    }
                }
            }
        });
    }

    /// A page carries *fields* exactly when the settings behind them take
    /// effect. Pinning this keeps "what is on screen works" from decaying into a
    /// screen full of dead controls.
    ///
    /// Keybindings has no fields on purpose and is not a counter-example: it
    /// reports assignments rather than offering settings, so it builds its own
    /// group (`groups_for`) and its strings are covered by
    /// `every_locale_carries_the_keybinding_list_keys`.
    #[test]
    fn only_the_pages_whose_settings_apply_carry_labels() {
        assert!(!label_keys(SettingsPageKind::General).is_empty());
        assert!(!label_keys(SettingsPageKind::Appearance).is_empty());
        assert!(!label_keys(SettingsPageKind::Language).is_empty());
        assert!(!label_keys(SettingsPageKind::Project).is_empty());
        assert!(label_keys(SettingsPageKind::Keybindings).is_empty());
    }

    /// The keybinding list's own strings, in every locale. Kept apart from
    /// `every_locale_carries_the_settings_dialog_keys` because these belong to a
    /// page rather than to the dialog shell — the shell's list should not grow a
    /// row every time a page gains a field.
    #[test]
    fn every_locale_carries_the_keybinding_list_keys() {
        let keys: Vec<&'static str> = [
            "settings.keybindings.group",
            "settings.keybindings.description",
        ]
        .into_iter()
        .chain(
            crate::keybindings::KeybindingOrigin::ALL
                .iter()
                .map(|origin| origin.label_key()),
        )
        .collect();

        for locale in ["en", "ja"] {
            let catalog = catalog(locale);
            for key in &keys {
                assert!(
                    has_key(&catalog, key),
                    "{locale}.toml is missing the keybinding list key \"{key}\""
                );
            }
        }
    }

    /// A panel-scoped row shows each key once and names every panel the key is
    /// confined to. `Delete` bound in two panels is one chip and two panel
    /// names, not two chips.
    #[test]
    fn a_panel_scoped_row_shows_each_key_once_and_names_its_panels() {
        let rows = crate::keybindings::rows(&crate::keybindings::read_keybindings_at(None));
        let find = |command: CommandId| {
            rows.iter()
                .find(|row| row.command == command)
                .expect("every command has a row")
        };

        let delete = find(CommandId::EditDelete);
        assert_eq!(
            chord_chips(delete),
            vec![
                SharedString::from("Delete"),
                SharedString::from("Backspace")
            ]
        );
        assert_eq!(
            confined_panels(delete),
            vec![PanelKind::NodeGraph, PanelKind::Timeline]
        );

        let pen = find(CommandId::ToolPen);
        assert_eq!(chord_chips(pen), vec![SharedString::from("P")]);
        assert_eq!(confined_panels(pen), vec![PanelKind::Viewer]);

        // A globally bound command shows its own chord and no panel names.
        let save = find(CommandId::FileSave);
        assert_eq!(chord_chips(save), vec![SharedString::from("Cmd+S")]);
        assert!(confined_panels(save).is_empty());

        // An unbound command shows no chip at all.
        assert!(chord_chips(find(CommandId::CompositionNew)).is_empty());
    }

    /// The list renders a panel's name for every panel the code-side table
    /// mentions, so those names have to exist in every locale too.
    #[test]
    fn every_locale_names_the_panels_the_list_mentions() {
        for locale in ["en", "ja"] {
            let catalog = catalog(locale);
            for binding in crate::workspace::PANEL_BINDINGS {
                let key = binding.panel.label_key();
                assert!(
                    has_key(&catalog, key),
                    "{locale}.toml is missing the panel name \"{key}\" the keybinding list renders"
                );
            }
        }
    }

    /// A frame rate the notation cannot read is warned about and ignored by
    /// [`app_settings::default_frame_rate`], so the row must not offer it as the
    /// current choice either — it would name a rate no composition is built at.
    ///
    /// Tested here rather than through the dialog because `SettingField`'s value
    /// getter is `pub(crate)` in `gpui_component` (`LOW-APP-20`), so the closure
    /// cannot be invoked from outside this crate.
    #[gpui::test]
    fn an_unreadable_frame_rate_setting_shows_the_rate_in_force(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "[playback]\nframe_rate = \"24fps\"\n").unwrap();
        cx.update(|cx| {
            crate::app_settings::install(
                crate::app_settings::read_global_settings_at(Some(path)),
                cx,
            );
        });

        cx.update(|cx| {
            let shown = frame_rate_option_in_force(cx);
            assert_eq!(
                shown, "30",
                "the row names the rate in force, not the text nothing can read"
            );
            assert_eq!(
                app_settings::parse_frame_rate(&shown),
                Some(app_settings::default_frame_rate(cx)),
                "and what it names parses back to the rate the reader fell back to"
            );
            // A readable value is shown as written, including one the list does
            // not carry.
            assert_eq!(
                COMMON_FRAME_RATES
                    .iter()
                    .filter(|option| app_settings::parse_frame_rate(option).is_none())
                    .count(),
                0,
                "every offered option has to parse, or the row could offer a dead rate"
            );
        });
    }

    /// Every page reaches the dialog with exactly one group, by either of the
    /// two routes [`groups_for`] joins: the field-based pages build theirs from
    /// [`fields_for`], and Keybindings builds its own.
    ///
    /// A page with no group is dropped by `Settings` and becomes unreachable, so
    /// this is the check that a page is wired at all. What the keybinding list
    /// *says* is pinned on `crate::keybindings::rows` instead — `SettingGroup`'s
    /// items are private to `gpui_component`, so its contents cannot be
    /// inspected from here.
    ///
    /// A gpui test rather than a plain one because `groups_for` reads the
    /// registry and the settings global to build the field-based pages.
    #[gpui::test]
    fn every_page_carries_exactly_one_group(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for page in SettingsPageKind::ALL {
                assert_eq!(groups_for(page, cx).len(), 1, "{page:?}");
            }
        });
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

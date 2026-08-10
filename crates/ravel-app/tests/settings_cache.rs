// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Preferences ▸ Cache, observed on what the four rows decide
//! (`docs/implementation/settings-screen-plan.md`, `SET-8`; REQ-PROJ-004).
//!
//! The page carries the limits and the location and nothing else. The disk
//! tier's own switch is deliberately absent: `Tier::Disk` is charged by
//! nothing, so a row for it would configure nothing until `CACHE-11` builds
//! the layer ("出す項目 = 効く項目").
//!
//! Every assertion here reads an eviction, a settings file or a cache
//! directory — never the state of a control. In particular the limit rows are
//! pinned by **crossing the ceiling**: a test that stays under the limit
//! passes just as well with the budget code dead (`cache-plan.md`).

use gpui::{
    AnyWindowHandle, AppContext as _, Context, IntoElement, Pixels, Render, SharedString, Size,
    Styled as _, TestAppContext, Window, div, px,
};
use ravel_app::app_settings::{self, SettingsScope, read_global_settings_at};
use ravel_app::media::cache::DiskCache;
use ravel_app::media::thumbnail::{
    ThumbnailCache, ThumbnailGenerator, ThumbnailSource, ThumbnailState,
};
use ravel_app::project_state::{
    ProjectState, ProjectStateHandle, disable_background_eval_for_tests,
};
use ravel_app::settings_dialog::{self, SettingsPageKind, fields_for};
use ravel_core::cache_budget::{CacheKind, SharedCacheBudget, Tier};
use ravel_core::color::ColorSpace;
use ravel_core::types::FrameBuffer;

/// Any window will do; a field's reset only needs one to exist.
const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(400.0),
    height: px(300.0),
};

const VRAM_ROW: &str = "settings.cache.vram_limit";
const RAM_ROW: &str = "settings.cache.ram_limit";
const SIM_ROW: &str = "settings.cache.sim_reserve";
const ROOT_ROW: &str = "settings.cache.root";

const MIB: u64 = 1024 * 1024;

/// A session with a project and an empty global settings file, plus the path
/// that file lives at — re-reading it is how "survives a relaunch" is checked.
fn start(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<ProjectState>,
    tempfile::TempDir,
    std::path::PathBuf,
) {
    disable_background_eval_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config").join("settings.toml");

    let project = cx.new(ProjectState::new);
    cx.update(|cx| {
        cx.set_global(ProjectStateHandle(project.downgrade()));
        app_settings::install(read_global_settings_at(Some(path.clone())), cx);
    });
    cx.run_until_parked();
    (project, dir, path)
}

/// Whether the row titled `title_key` offers a reset.
fn row_is_resettable(title_key: &str, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| {
        fields_for(SettingsPageKind::Cache, cx)
            .into_iter()
            .find(|page_field| page_field.title_key == title_key)
            .unwrap_or_else(|| panic!("the Cache page has no row {title_key:?}"))
            .field
            .any()
            .is_resettable(cx)
    })
}

/// Invoke the reset the dialog would invoke for that row.
fn reset_row(window: AnyWindowHandle, title_key: &str, cx: &mut TestAppContext) {
    window
        .update(cx, |_view, window, cx| {
            let page_field = fields_for(SettingsPageKind::Cache, cx)
                .into_iter()
                .find(|page_field| page_field.title_key == title_key)
                .unwrap_or_else(|| panic!("the Cache page has no row {title_key:?}"));
            page_field.field.any().reset(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// **The completion criterion for the limit rows**: lowering one through the
/// row's own setter makes the running budget evict.
///
/// A budget of this test's own rather than the session's, so what is pinned is
/// the conversion and the eviction and not the lookup that finds a session —
/// `a_settings_edit_moves_the_ceiling_of_the_session_budget` pins that half.
///
/// The reservation deliberately **crosses** the new ceiling: at the default
/// 2 GiB it is comfortably resident, and at 64 MiB it is not. A test that
/// stayed under both limits would pass with `reconfigure` deleted.
#[gpui::test]
fn lowering_the_memory_limit_evicts_what_no_longer_fits(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);
    let budget = cx.update(|cx| {
        let budget = SharedCacheBudget::new(app_settings::resolved(cx).cache_budget());
        app_settings::apply_cache_budget(&budget, cx);
        budget
    });

    let held = budget
        .reserve(CacheKind::NodeResult(Tier::Ram), 100 * MIB)
        .0;
    let (_next, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), MIB);
    assert!(
        evicted.is_empty(),
        "100 MiB is resident under the default 2 GiB ceiling"
    );

    cx.update(|cx| settings_dialog::set_cache_ram_limit_mb(64.0, cx));
    cx.run_until_parked();
    cx.update(|cx| app_settings::apply_cache_budget(&budget, cx));

    let (_after, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), MIB);
    assert_eq!(
        evicted.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![held.id()],
        "the entry that no longer fits under the new ceiling is dropped"
    );
    assert_eq!(budget.stats().limit(Tier::Ram), 64 * MIB);
    drop(held);

    // The VRAM row moves its own tier and nothing else.
    cx.update(|cx| settings_dialog::set_cache_vram_limit_mb(128.0, cx));
    cx.run_until_parked();
    cx.update(|cx| app_settings::apply_cache_budget(&budget, cx));
    assert_eq!(budget.stats().limit(Tier::Vram), 128 * MIB);
    assert_eq!(budget.stats().limit(Tier::Ram), 64 * MIB);
}

/// **The completion criterion for the wiring itself**: nobody has to call the
/// apply path. Editing a row moves the ceiling of the budget the *session*
/// runs on — the one `ProjectState` hands the evaluation worker — and so does
/// a project layer arriving with a document.
///
/// The session's budget is reached through the same handle every settings
/// consumer uses, and the apply is deferred (the project layer is adopted from
/// inside a `ProjectState` update), so each step ends on `run_until_parked`.
#[gpui::test]
fn a_settings_edit_moves_the_ceiling_of_the_session_budget(cx: &mut TestAppContext) {
    let (project, _dir, _path) = start(cx);
    let budget = project.read_with(cx, |project, _| project.cache_budget().clone());
    assert_eq!(
        budget.stats().limit(Tier::Ram),
        ravel_project::settings::ResolvedSettings::default().cache_ram_limit_mb * MIB,
        "a session starts on the limits its settings resolve to"
    );

    cx.update(|cx| settings_dialog::set_cache_ram_limit_mb(96.0, cx));
    cx.run_until_parked();
    assert_eq!(
        budget.stats().limit(Tier::Ram),
        96 * MIB,
        "a preferences edit reaches the running budget without anything else asking"
    );

    // A project layer overrides it as the document opens, and stops applying
    // when the document is replaced by one that overrides nothing.
    cx.update(|cx| {
        app_settings::update(
            app_settings::SettingsScope::Project,
            |layer| layer.cache.ram_limit_mb = Some(32),
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(budget.stats().limit(Tier::Ram), 32 * MIB);

    project.update(cx, |project, cx| project.new_document(cx));
    cx.run_until_parked();
    assert_eq!(
        budget.stats().limit(Tier::Ram),
        96 * MIB,
        "the closing project's cache override stops applying with it"
    );
}

/// The sim reserve holds bytes back from ordinary entries, so moving it is
/// observable as an ordinary reservation being refused room it had before.
#[gpui::test]
fn the_simulation_reserve_holds_its_share_back_from_ordinary_entries(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);
    cx.update(|cx| {
        settings_dialog::set_cache_ram_limit_mb(100.0, cx);
        settings_dialog::set_cache_sim_reserve_ratio(0.0, cx);
    });
    cx.run_until_parked();
    let budget = cx.update(|cx| {
        let budget = SharedCacheBudget::new(app_settings::resolved(cx).cache_budget());
        app_settings::apply_cache_budget(&budget, cx);
        budget
    });

    let held = budget.reserve(CacheKind::NodeResult(Tier::Ram), 80 * MIB).0;
    let (small, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 10 * MIB);
    assert!(evicted.is_empty(), "90 of 100 MiB fits with no reserve");
    drop((held, small));

    cx.update(|cx| settings_dialog::set_cache_sim_reserve_ratio(0.5, cx));
    cx.run_until_parked();
    cx.update(|cx| app_settings::apply_cache_budget(&budget, cx));

    let held = budget.reserve(CacheKind::NodeResult(Tier::Ram), 40 * MIB).0;
    let (_small, evicted) = budget.reserve(CacheKind::NodeResult(Tier::Ram), 20 * MIB);
    assert_eq!(
        evicted.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![held.id()],
        "half the tier is reserved, so 60 MiB of ordinary entries no longer fit"
    );
    drop(held);
}

/// **The completion criterion for the location row**: a `cache.root` the
/// settings hold is where a cache puts its files.
///
/// Driven through `ThumbnailCache`, which is the cache the application
/// actually builds from this setting (`panels::media_bin`), with a stub
/// decoder so the test needs no media. The generated thumbnail has to land
/// under the configured root and nowhere near the config directory.
#[gpui::test]
fn a_configured_cache_location_is_where_the_thumbnail_cache_writes(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("clip.mov");
    std::fs::write(&source, b"media fixture").unwrap();

    // Unset, the location is the platform config directory, not the source's
    // own directory — the row is an override, not the only answer.
    assert_eq!(
        cx.update(|cx| app_settings::cache_root(cx)),
        ravel_project::paths::global_config_dir()
    );

    cx.update(|cx| {
        settings_dialog::set_cache_root(SharedString::from(root.path().to_str().unwrap()), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| app_settings::cache_root(cx)),
        Some(root.path().to_path_buf())
    );

    let generator: ThumbnailGenerator = std::sync::Arc::new(|_path, _source, _space| {
        Ok(FrameBuffer::from_f32(2, 2, vec![1.0; 16]))
    });
    let cache = cx.update(|cx| {
        let root = app_settings::cache_root(cx);
        cx.new(|_| ThumbnailCache::with_generator(root, generator))
    });
    cache.update(cx, |cache, cx| {
        cache.get_or_request(&source, ThumbnailSource::Container, ColorSpace::SRGB, cx)
    });
    cx.run_until_parked();
    assert!(matches!(
        cache.update(cx, |cache, cx| cache.get_or_request(
            &source,
            ThumbnailSource::Container,
            ColorSpace::SRGB,
            cx
        )),
        ThumbnailState::Ready(_)
    ));

    let written = std::fs::read_dir(root.path().join("cache").join("thumbnails"))
        .expect("the configured location holds the thumbnail cache directory")
        .count();
    assert_eq!(written, 1, "the thumbnail was written under the setting");
}

/// A location the settings cannot use is refused rather than written: an empty
/// field is "no location of my own" (the same state the reset leaves), and a
/// relative path would resolve against a working directory the user never
/// chose.
#[gpui::test]
fn an_unusable_cache_location_is_refused(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);
    let root = tempfile::tempdir().unwrap();
    let absolute = SharedString::from(root.path().to_str().unwrap());

    cx.update(|cx| settings_dialog::set_cache_root(absolute.clone(), cx));
    cx.run_until_parked();

    for refused in ["relative/cache", "./cache"] {
        cx.update(|cx| settings_dialog::set_cache_root(SharedString::from(refused), cx));
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| app_settings::layer(SettingsScope::Global, cx))
                .cache
                .root
                .as_deref(),
            Some(absolute.as_ref()),
            "{refused:?} left the previous location in force"
        );
    }

    // An empty field drops the override rather than writing a path that would
    // resolve against the working directory.
    cx.update(|cx| settings_dialog::set_cache_root(SharedString::from("  "), cx));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Global, cx))
            .cache
            .root,
        None
    );
    assert_eq!(
        cx.update(|cx| app_settings::cache_root(cx)),
        ravel_project::paths::global_config_dir()
    );
}

/// A limit the tiers cannot hold is refused rather than clamped or zeroed.
/// These are numbers a person types, and one that silently became something
/// else is a cache nobody can reason about.
#[gpui::test]
fn an_out_of_range_limit_is_refused_rather_than_clamped(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);
    cx.update(|cx| {
        settings_dialog::set_cache_vram_limit_mb(512.0, cx);
        settings_dialog::set_cache_ram_limit_mb(512.0, cx);
        settings_dialog::set_cache_sim_reserve_ratio(0.25, cx);
    });
    cx.run_until_parked();

    let refused = [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        settings_dialog::MAX_CACHE_LIMIT_MB + 1.0,
    ];
    for value in refused {
        cx.update(|cx| {
            settings_dialog::set_cache_vram_limit_mb(value, cx);
            settings_dialog::set_cache_ram_limit_mb(value, cx);
        });
        cx.run_until_parked();
        let resolved = cx.update(|cx| app_settings::resolved(cx));
        assert_eq!(resolved.cache_vram_limit_mb, 512, "VRAM took {value}");
        assert_eq!(resolved.cache_ram_limit_mb, 512, "RAM took {value}");
    }

    for value in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
        cx.update(|cx| settings_dialog::set_cache_sim_reserve_ratio(value, cx));
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| app_settings::resolved(cx))
                .cache_sim_reserve_ratio,
            0.25,
            "the reserve took {value}"
        );
    }

    // The bounds themselves are accepted, so the range is a range and not an
    // accidental exclusion of every usable value.
    cx.update(|cx| {
        settings_dialog::set_cache_vram_limit_mb(settings_dialog::MIN_CACHE_LIMIT_MB, cx);
        settings_dialog::set_cache_ram_limit_mb(settings_dialog::MAX_CACHE_LIMIT_MB, cx);
        settings_dialog::set_cache_sim_reserve_ratio(1.0, cx);
    });
    cx.run_until_parked();
    let resolved = cx.update(|cx| app_settings::resolved(cx));
    assert_eq!(resolved.cache_vram_limit_mb, 1);
    assert_eq!(resolved.cache_ram_limit_mb, 1024 * 1024);
    assert_eq!(resolved.cache_sim_reserve_ratio, 1.0);
}

/// Every row writes the **global** layer: cache limits are preferences of the
/// machine, not properties of a document, so the criterion is "survives a
/// restart" rather than "is held in memory".
#[gpui::test]
fn every_row_writes_the_global_layer(cx: &mut TestAppContext) {
    let (_project, _dir, path) = start(cx);
    let root = tempfile::tempdir().unwrap();

    cx.update(|cx| {
        settings_dialog::set_cache_vram_limit_mb(256.0, cx);
        settings_dialog::set_cache_ram_limit_mb(768.0, cx);
        settings_dialog::set_cache_sim_reserve_ratio(0.4, cx);
        settings_dialog::set_cache_root(SharedString::from(root.path().to_str().unwrap()), cx);
    });
    cx.run_until_parked();

    let reread = read_global_settings_at(Some(path)).resolved();
    assert_eq!(reread.cache_vram_limit_mb, 256);
    assert_eq!(reread.cache_ram_limit_mb, 768);
    assert_eq!(reread.cache_sim_reserve_ratio, 0.4);
    assert_eq!(reread.cache_root.as_deref(), root.path().to_str());
    // And the file the next launch reads implies the same budget.
    assert_eq!(reread.cache_budget().ram_bytes, 768 * MIB);
}

/// "Reset to default" removes the value from the layer rather than writing the
/// default back as an explicit choice (which is what `default_value()` would
/// do, and why the plan bans it). A file that no longer mentions the setting is
/// the observable difference.
#[gpui::test]
fn resetting_a_row_drops_the_override_rather_than_writing_the_default(cx: &mut TestAppContext) {
    let (_project, _dir, path) = start(cx);
    let window: AnyWindowHandle = cx.open_window(WINDOW_SIZE, |_window, _cx| Blank).into();
    let root = tempfile::tempdir().unwrap();
    let rows = [VRAM_ROW, RAM_ROW, SIM_ROW, ROOT_ROW];

    for row in rows {
        assert!(
            !row_is_resettable(row, cx),
            "an untouched preference is not an override, so {row} has nothing to reset"
        );
    }

    let defaults = ravel_project::settings::ResolvedSettings::default();
    cx.update(|cx| {
        // The values the defaults already hold: the layer now carries an
        // explicit value, which is a different state from "not overridden".
        settings_dialog::set_cache_vram_limit_mb(defaults.cache_vram_limit_mb as f64, cx);
        settings_dialog::set_cache_ram_limit_mb(defaults.cache_ram_limit_mb as f64, cx);
        settings_dialog::set_cache_sim_reserve_ratio(
            f64::from(defaults.cache_sim_reserve_ratio),
            cx,
        );
        settings_dialog::set_cache_root(SharedString::from(root.path().to_str().unwrap()), cx);
    });
    cx.run_until_parked();
    for row in rows {
        assert!(row_is_resettable(row, cx), "{row} now holds an override");
    }

    for row in rows {
        reset_row(window, row, cx);
    }

    let layer = cx.update(|cx| app_settings::layer(SettingsScope::Global, cx));
    assert_eq!(layer.cache.vram_limit_mb, None);
    assert_eq!(layer.cache.ram_limit_mb, None);
    assert_eq!(layer.cache.sim_reserve_ratio, None);
    assert_eq!(layer.cache.root, None);
    for row in rows {
        assert!(!row_is_resettable(row, cx), "{row} is an override again");
    }

    let text = std::fs::read_to_string(&path).expect("the global layer was written");
    assert!(
        !text.contains("vram_limit_mb")
            && !text.contains("ram_limit_mb")
            && !text.contains("sim_reserve_ratio")
            && !text.contains("root"),
        "the reset removed the values instead of writing the defaults back: {text}"
    );
}

/// The disk tier stays out of the page while nothing charges it: the layer
/// carries the two fields so a project can be written against them
/// (`CACHE-11`), but a row would offer a setting that changes nothing.
#[gpui::test]
fn the_disk_tier_is_not_offered_while_nothing_charges_it(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);
    let titles: Vec<&str> = cx.update(|cx| {
        fields_for(SettingsPageKind::Cache, cx)
            .into_iter()
            .map(|field| field.title_key)
            .collect()
    });
    assert_eq!(titles, vec![VRAM_ROW, RAM_ROW, SIM_ROW, ROOT_ROW]);
    // And the default the resolved settings hand the budget keeps the tier at
    // zero, so nothing can be spilled into a layer that does not exist.
    assert_eq!(
        cx.update(|cx| app_settings::resolved(cx))
            .cache_budget()
            .disk_bytes,
        0
    );
}

/// A window has to have a root; this one has nothing else to do.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

/// `DiskCache` is what every cache in the application puts its files through,
/// so the location setting has to be readable as a plain path decision too —
/// the thumbnail test above pins the one caller, this pins the shape any
/// future one inherits.
#[gpui::test]
fn the_location_setting_is_the_root_a_disk_cache_is_built_on(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);
    let root = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        settings_dialog::set_cache_root(SharedString::from(root.path().to_str().unwrap()), cx);
    });
    cx.run_until_parked();

    let source = root.path().join("asset.wav");
    std::fs::write(&source, b"fixture").unwrap();
    let cache = cx.update(|cx| DiskCache::new(app_settings::cache_root(cx), "waveforms"));
    let key = DiskCache::key(&source, "").expect("an absolute path keys");
    cache.store(&key, b"derived").expect("the entry is written");

    assert_eq!(cache.load(&key).as_deref(), Some(b"derived".as_slice()));
    assert!(root.path().join("cache").join("waveforms").is_dir());
}

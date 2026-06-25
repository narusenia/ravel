// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lightweight i18n system for Ravel.
//!
//! Loads TOML locale files, resolves dotted keys via the [`t!`] macro,
//! and supports runtime language switching with English fallback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use anyhow::{Context, Result};

/// Global locale store, lazily initialized.
fn store() -> &'static RwLock<LocaleStore> {
    static STORE: OnceLock<RwLock<LocaleStore>> = OnceLock::new();
    STORE.get_or_init(|| {
        RwLock::new(LocaleStore {
            current: String::new(),
            catalogs: HashMap::new(),
            locale_dir: None,
        })
    })
}

/// A flattened locale catalog: `"menu.file.new"` → `"New"`.
type Catalog = HashMap<String, String>;

struct LocaleStore {
    current: String,
    catalogs: HashMap<String, Catalog>,
    locale_dir: Option<PathBuf>,
}

impl LocaleStore {
    fn get(&self, key: &str) -> String {
        if let Some(catalog) = self.catalogs.get(&self.current)
            && let Some(val) = catalog.get(key)
        {
            return val.clone();
        }
        if self.current != "en"
            && let Some(en) = self.catalogs.get("en")
            && let Some(val) = en.get(key)
        {
            return val.clone();
        }
        key.to_string()
    }
}

/// Looks up a translation key in the current locale.
///
/// Falls back to English, then to the raw key.
pub fn translate(key: &str) -> String {
    store().read().expect("i18n lock poisoned").get(key)
}

/// Returns the currently active locale code (e.g. `"en"`, `"ja"`).
pub fn current_locale() -> String {
    store().read().expect("i18n lock poisoned").current.clone()
}

/// Returns the list of loaded locale codes.
pub fn available_locales() -> Vec<String> {
    store()
        .read()
        .expect("i18n lock poisoned")
        .catalogs
        .keys()
        .cloned()
        .collect()
}

/// Initializes the i18n system by loading all `*.toml` files from `locale_dir`.
///
/// Sets the active locale to `default_locale`. Call this once at startup before
/// any `t!()` invocations.
pub fn init(locale_dir: &Path, default_locale: &str) -> Result<()> {
    let mut store = store().write().expect("i18n lock poisoned");
    store.locale_dir = Some(locale_dir.to_path_buf());
    store.catalogs.clear();

    let entries = std::fs::read_dir(locale_dir)
        .with_context(|| format!("reading locale dir: {}", locale_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            let catalog = load_toml_catalog(&path)?;
            tracing::info!(locale = stem, keys = catalog.len(), "loaded locale");
            store.catalogs.insert(stem.to_string(), catalog);
        }
    }

    if !store.catalogs.contains_key(default_locale) {
        anyhow::bail!(
            "default locale '{}' not found in {}",
            default_locale,
            locale_dir.display()
        );
    }

    store.current = default_locale.to_string();
    Ok(())
}

/// Switches the active locale at runtime.
///
/// Returns an error if the locale has not been loaded.
pub fn set_locale(locale: &str) -> Result<()> {
    let mut store = store().write().expect("i18n lock poisoned");
    if !store.catalogs.contains_key(locale) {
        // Try to load it from the locale dir
        if let Some(dir) = &store.locale_dir {
            let path = dir.join(format!("{locale}.toml"));
            if path.exists() {
                let catalog = load_toml_catalog(&path)?;
                tracing::info!(locale, keys = catalog.len(), "loaded locale on demand");
                store.catalogs.insert(locale.to_string(), catalog);
            } else {
                anyhow::bail!("locale '{locale}' not found");
            }
        } else {
            anyhow::bail!("locale '{locale}' not loaded and no locale dir configured");
        }
    }
    store.current = locale.to_string();
    Ok(())
}

/// Loads a TOML file and flattens nested tables into dot-separated keys.
fn load_toml_catalog(path: &Path) -> Result<Catalog> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let table: toml::Table = content
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    let mut catalog = Catalog::new();
    flatten_toml(&table, &mut String::new(), &mut catalog);
    Ok(catalog)
}

/// Recursively flattens a TOML table into dot-separated keys.
///
/// `[menu.file] new = "New"` becomes `"menu.file.new"` → `"New"`.
/// Keys named `_self` are stored without the suffix, so
/// `[panel.properties] _self = "Properties"` becomes `"panel.properties"`.
fn flatten_toml(table: &toml::Table, prefix: &mut String, out: &mut Catalog) {
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else if key == "_self" {
            prefix.clone()
        } else {
            format!("{prefix}.{key}")
        };

        match value {
            toml::Value::String(s) => {
                out.insert(full_key, s.clone());
            }
            toml::Value::Table(sub) => {
                flatten_toml(sub, &mut full_key.clone(), out);
            }
            _ => {
                tracing::warn!(key = %full_key, "ignoring non-string/table value in locale");
            }
        }
    }
}

/// Translates a dotted i18n key using the current locale.
///
/// Falls back to the English locale, then to the raw key.
///
/// # Examples
///
/// ```ignore
/// use ravel_i18n::t;
/// let label = t!("menu.file.new"); // "New" (en) or "新規" (ja)
/// ```
#[macro_export]
macro_rules! t {
    ($key:expr) => {
        $crate::translate($key)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn setup_test_locales(dir: &Path) {
        let en = dir.join("en.toml");
        let ja = dir.join("ja.toml");

        let mut f = std::fs::File::create(&en).unwrap();
        writeln!(
            f,
            r#"
[menu.file]
new = "New"
open = "Open…"

[panel]
outliner = "Outliner"

[panel.properties]
_self = "Properties"
empty = "No node selected"
"#
        )
        .unwrap();

        let mut f = std::fs::File::create(&ja).unwrap();
        writeln!(
            f,
            r#"
[menu.file]
new = "新規"
open = "開く…"

[panel]
outliner = "アウトライナー"

[panel.properties]
_self = "プロパティ"
empty = "ノード未選択"
"#
        )
        .unwrap();
    }

    #[test]
    fn translate_english() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        setup_test_locales(dir.path());
        init(dir.path(), "en").unwrap();

        assert_eq!(translate("menu.file.new"), "New");
        assert_eq!(translate("panel.outliner"), "Outliner");
        assert_eq!(translate("panel.properties"), "Properties");
        assert_eq!(translate("panel.properties.empty"), "No node selected");
    }

    #[test]
    fn translate_japanese_with_fallback() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        setup_test_locales(dir.path());
        init(dir.path(), "ja").unwrap();

        assert_eq!(translate("menu.file.new"), "新規");
        assert_eq!(translate("nonexistent.key"), "nonexistent.key");
    }

    #[test]
    fn set_locale_switches_language() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        setup_test_locales(dir.path());
        init(dir.path(), "en").unwrap();

        assert_eq!(translate("menu.file.new"), "New");
        set_locale("ja").unwrap();
        assert_eq!(translate("menu.file.new"), "新規");
        set_locale("en").unwrap();
        assert_eq!(translate("menu.file.new"), "New");
    }

    #[test]
    fn self_key_flattens_correctly() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        setup_test_locales(dir.path());
        init(dir.path(), "en").unwrap();

        assert_eq!(translate("panel.properties"), "Properties");
        assert_eq!(translate("panel.properties.empty"), "No node selected");
    }

    #[test]
    fn t_macro_works() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        setup_test_locales(dir.path());
        init(dir.path(), "en").unwrap();

        let val = t!("menu.file.new");
        assert_eq!(val, "New");
    }

    #[test]
    fn missing_locale_errors() {
        let _lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        setup_test_locales(dir.path());
        assert!(init(dir.path(), "fr").is_err());
    }
}

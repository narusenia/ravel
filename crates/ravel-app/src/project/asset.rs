// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset reference system (`assets/refs.json`).
//!
//! Ravel never embeds media inside a `.ravprj`; it stores **references** only
//! (see the data-model spec). A reference is either a project-relative path or
//! a variable-prefixed path such as `${PROJECT_ROOT}/footage/clip.mov`. Both
//! forms resolve to an absolute [`PathBuf`] through [`AssetPath::resolve`],
//! which expands `${VAR}` tokens from a caller-supplied table (typically
//! seeded with `PROJECT_ROOT` and the process environment).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::project::manifest::RationalRate;

/// Stable identifier for an asset within a project.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub String);

/// Location of an asset's backing file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssetPath {
    /// Path relative to the project root, e.g. `"./footage/clip01.mov"`.
    Relative { rel: String },
    /// Variable-prefixed path, e.g. `("${PROJECT_ROOT}", "footage/clip01.mov")`.
    Variable { var: String, rel: String },
}

impl AssetPath {
    /// Resolve this path to an absolute location.
    ///
    /// `project_root` anchors [`AssetPath::Relative`] paths and is also exposed
    /// to variable paths as `${PROJECT_ROOT}`. `vars` supplies additional
    /// substitution values (e.g. environment variables); entries in `vars`
    /// take precedence over the implicit `PROJECT_ROOT`.
    pub fn resolve(&self, project_root: &Path, vars: &HashMap<String, String>) -> PathBuf {
        match self {
            AssetPath::Relative { rel } => project_root.join(strip_leading_dot(rel)),
            AssetPath::Variable { var, rel } => {
                let mut table = HashMap::new();
                table.insert(
                    "PROJECT_ROOT".to_string(),
                    project_root.to_string_lossy().into_owned(),
                );
                for (k, v) in vars {
                    table.insert(k.clone(), v.clone());
                }
                let base = expand_variables(var, &table);
                PathBuf::from(base).join(strip_leading_dot(rel))
            }
        }
    }
}

/// Expand `${NAME}` tokens in `input` using `vars`.
///
/// Unknown tokens are left verbatim so that resolution is lossless and
/// debuggable rather than silently dropping path segments. The scan is
/// single-pass and never panics on unbalanced braces.
pub fn expand_variables(input: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'{'
            && let Some(end_rel) = input[i + 2..].find('}')
        {
            let end = i + 2 + end_rel;
            let name = &input[i + 2..end];
            match vars.get(name) {
                Some(value) => out.push_str(value),
                // Preserve the original token when unknown.
                None => out.push_str(&input[i..=end]),
            }
            i = end + 1;
            continue;
        }
        // Copy one UTF-8 character intact.
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&input[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn strip_leading_dot(rel: &str) -> &str {
    rel.strip_prefix("./").unwrap_or(rel)
}

fn utf8_char_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Proxy generation status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyStatus {
    Pending,
    Ready,
    Failed,
}

/// Low-resolution proxy media descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub path: AssetPath,
    /// `0.5` = half resolution, `0.25` = quarter, …
    pub resolution_factor: f32,
    pub status: ProxyStatus,
}

/// Decoded metadata cached alongside the reference.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssetMetadata {
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub frame_rate: Option<RationalRate>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub color_space: Option<String>,
    #[serde(default)]
    pub file_size: u64,
}

/// A single asset reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetRef {
    pub id: AssetId,
    pub path: AssetPath,
    /// Integrity hash, e.g. `"sha256:abcdef…"`.
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub proxy: Option<ProxyInfo>,
    #[serde(default)]
    pub metadata: AssetMetadata,
}

/// The full set of asset references for a project (`assets/refs.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssetCollection {
    #[serde(default)]
    pub assets: Vec<AssetRef>,
}

impl AssetCollection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serialize to pretty JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse from JSON.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Look up an asset by id.
    pub fn get(&self, id: &AssetId) -> Option<&AssetRef> {
        self.assets.iter().find(|a| &a.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn relative_path_resolves_against_root() {
        let p = AssetPath::Relative {
            rel: "./footage/clip.mov".into(),
        };
        let resolved = p.resolve(Path::new("/projects/demo"), &HashMap::new());
        assert_eq!(resolved, PathBuf::from("/projects/demo/footage/clip.mov"));
    }

    #[test]
    fn variable_path_expands_project_root() {
        let p = AssetPath::Variable {
            var: "${PROJECT_ROOT}".into(),
            rel: "footage/bg.mov".into(),
        };
        let resolved = p.resolve(Path::new("/abs/proj"), &HashMap::new());
        assert_eq!(resolved, PathBuf::from("/abs/proj/footage/bg.mov"));
    }

    #[test]
    fn variable_path_expands_custom_var() {
        let p = AssetPath::Variable {
            var: "${MEDIA}".into(),
            rel: "a/b.mov".into(),
        };
        let resolved = p.resolve(Path::new("/proj"), &vars(&[("MEDIA", "/mnt/media")]));
        assert_eq!(resolved, PathBuf::from("/mnt/media/a/b.mov"));
    }

    #[test]
    fn unknown_variable_is_preserved() {
        let out = expand_variables("${UNKNOWN}/tail", &HashMap::new());
        assert_eq!(out, "${UNKNOWN}/tail");
    }

    #[test]
    fn expand_handles_unbalanced_braces() {
        // Must not panic; leaves the dangling token intact.
        assert_eq!(expand_variables("${NOPE", &HashMap::new()), "${NOPE");
        assert_eq!(
            expand_variables("plain $ text", &HashMap::new()),
            "plain $ text"
        );
    }

    #[test]
    fn expand_handles_multibyte_text() {
        let out = expand_variables("日本語${X}テキスト", &vars(&[("X", "値")]));
        assert_eq!(out, "日本語値テキスト");
    }

    #[test]
    fn collection_json_roundtrip() {
        let collection = AssetCollection {
            assets: vec![AssetRef {
                id: AssetId("asset_001".into()),
                path: AssetPath::Variable {
                    var: "${PROJECT_ROOT}".into(),
                    rel: "footage/bg.mov".into(),
                },
                hash: Some("sha256:abc".into()),
                proxy: Some(ProxyInfo {
                    path: AssetPath::Relative {
                        rel: "./proxies/bg.mov".into(),
                    },
                    resolution_factor: 0.5,
                    status: ProxyStatus::Ready,
                }),
                metadata: AssetMetadata {
                    width: Some(1920),
                    height: Some(1080),
                    frame_rate: Some(RationalRate::new(30, 1)),
                    codec: Some("h264".into()),
                    color_space: Some("sRGB".into()),
                    file_size: 104_857_600,
                },
            }],
        };
        let json = collection.to_json().unwrap();
        let back = AssetCollection::from_json(&json).unwrap();
        assert_eq!(collection, back);
        assert!(back.get(&AssetId("asset_001".into())).is_some());
    }

    #[test]
    fn malformed_json_is_error() {
        assert!(AssetCollection::from_json("{ not json").is_err());
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless row model for the MediaBin panel (REQ-UI-008, media-import plan
//! unit 4).
//!
//! The MediaBin lists the project's media assets — one flat row per entry of
//! [`Document::media_assets`], narrowed by a kind filter and a name search.
//! This module builds those rows from a `Document` snapshot so the GPUI host
//! paints a list and never probes, decodes, or walks a graph inside
//! `render()`. It also owns the reference scan the delete confirmation needs:
//! which layers of which compositions still use an asset.
//!
//! The panel holds no selection — that is the host's `MediaSelection` global,
//! the same split as the Outliner's `LayerSelection` (REQ-UI-013).

use ravel_core::composition::{AssetKind, Document, MediaAssetEntry};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::id::{CompId, LayerId};
use std::path::Path;

use crate::panel::PanelKind;

/// The kind filter of the MediaBin list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaBinFilter {
    /// Every asset.
    #[default]
    All,
    /// Video containers (with or without audio) and image sequences.
    Video,
    /// Single images.
    Still,
    /// Audio-only containers.
    Audio,
}

/// The display category of a media row — what the filter and the type icon
/// classify an asset as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaBinRowKind {
    /// Plays over time: a video container or an image sequence.
    Video,
    /// A single image.
    Still,
    /// An audio-only container.
    Audio,
}

/// Classify an asset into its display category.
///
/// A sequence is video (it plays over time). A container is **audio** only
/// when it has at least one audio stream and no probed video stream; a
/// container holding both stays video. "Has video" reads the probed
/// resolution: the metadata dimensions come from the first video stream, so
/// a video-less container has none. A container whose probe failed carries
/// no dimensions *and* no audio count, and so stays video — the importable
/// default.
pub fn classify(entry: &MediaAssetEntry) -> MediaBinRowKind {
    match &entry.kind {
        AssetKind::Still => MediaBinRowKind::Still,
        AssetKind::Sequence { .. } => MediaBinRowKind::Video,
        AssetKind::Container => {
            if entry.metadata.has_audio() && entry.metadata.width.is_none() {
                MediaBinRowKind::Audio
            } else {
                MediaBinRowKind::Video
            }
        }
    }
}

/// One visible line of the MediaBin list.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaBinRow {
    /// Key of [`Document::media_assets`].
    pub asset_id: String,
    /// Display name: the file name of the persisted path (the asset id when
    /// the path yields none).
    pub name: String,
    pub kind: MediaBinRowKind,
    /// Probed duration in seconds, when the import recorded one.
    pub duration: Option<f64>,
    /// The asset has no location on disk (`resolved == None`).
    pub offline: bool,
}

/// Headless MediaBin state: the kind filter and the search query, and the
/// flattening of a document into rows.
#[derive(Clone, Debug, Default)]
pub struct MediaBinPanel {
    filter: MediaBinFilter,
    query: String,
}

impl MediaBinPanel {
    pub const KIND: PanelKind = PanelKind::MediaBin;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn filter(&self) -> MediaBinFilter {
        self.filter
    }

    pub fn set_filter(&mut self, filter: MediaBinFilter) {
        self.filter = filter;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
    }

    /// Flatten `document` into the visible rows: assets passing the kind
    /// filter and the (case-insensitive, substring) name search, sorted by
    /// name with the asset id as the stable tie-break.
    pub fn rows(&self, document: &Document) -> Vec<MediaBinRow> {
        let query = self.query.trim().to_lowercase();
        let mut rows: Vec<MediaBinRow> = document
            .media_assets
            .iter()
            .map(|(id, entry)| MediaBinRow {
                asset_id: id.clone(),
                name: asset_name(id, entry),
                kind: classify(entry),
                duration: entry.metadata.duration_secs,
                offline: entry.is_offline(),
            })
            .filter(|row| self.matches_filter(row.kind))
            .filter(|row| query.is_empty() || row.name.to_lowercase().contains(&query))
            .collect();
        rows.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.asset_id.cmp(&b.asset_id))
        });
        rows
    }

    fn matches_filter(&self, kind: MediaBinRowKind) -> bool {
        match self.filter {
            MediaBinFilter::All => true,
            MediaBinFilter::Video => kind == MediaBinRowKind::Video,
            MediaBinFilter::Still => kind == MediaBinRowKind::Still,
            MediaBinFilter::Audio => kind == MediaBinRowKind::Audio,
        }
    }
}

/// The display name of an asset: the file name of its persisted path. The
/// persisted form is one string for every variant (`AssetPath` Display), and
/// a leading `${VAR}` component still leaves a usable file name; the asset
/// id is the fallback when the path has no real file name (empty, trailing
/// separator, or a bare variable token).
fn asset_name(id: &str, entry: &MediaAssetEntry) -> String {
    let text = entry.path.to_string();
    Path::new(&text)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty() && !name.starts_with("${"))
        .unwrap_or_else(|| id.to_string())
}

/// One layer referencing an asset, named by its composition for the delete
/// confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetReference {
    pub comp: CompId,
    pub layer: LayerId,
}

/// Every layer that still uses `asset_id` — a `media`/`video` node bound to
/// it in the layer network, or the layer shell's audio source — in
/// composition display order (comps by id, layers in stack order). A layer
/// appears once even when both paths reference the asset.
pub fn asset_references(document: &Document, asset_id: &str) -> Vec<AssetReference> {
    let mut comps: Vec<_> = document.compositions.values().collect();
    comps.sort_by_key(|comp| comp.id);

    let mut references = Vec::new();
    for comp in comps {
        for layer in &comp.layers {
            let used_by_node = layer
                .network
                .nodes()
                .any(|node| node_uses_asset(node, asset_id));
            let used_by_audio = layer
                .audio
                .as_ref()
                .is_some_and(|audio| audio.asset_id == asset_id);
            if used_by_node || used_by_audio {
                references.push(AssetReference {
                    comp: comp.id,
                    layer: layer.id,
                });
            }
        }
    }
    references
}

/// Whether `node` is a media node bound to `asset_id` (the binding
/// `add_media_layer` writes; `video` is the persisted alias of `media`).
fn node_uses_asset(node: &Node, asset_id: &str) -> bool {
    matches!(node.type_key.as_str(), "media" | "video")
        && node.parameters.iter().any(|param| {
            param.key == "asset_id"
                && matches!(&param.value, ParameterValue::String(value) if value == asset_id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{AssetMetadata, AudioSource, Composition, Layer};
    use ravel_core::graph::Graph;
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameRate;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn container_entry(width: Option<u32>, audio: usize, duration: Option<f64>) -> MediaAssetEntry {
        let mut entry = MediaAssetEntry::from_absolute("/media/clip.mov");
        entry.metadata = AssetMetadata {
            width,
            height: width.map(|_| 1080),
            duration_secs: duration,
            audio_stream_count: audio,
            ..AssetMetadata::default()
        };
        entry
    }

    fn document_with(assets: Vec<(&str, MediaAssetEntry)>) -> Document {
        let mut doc = Document::default();
        for (id, entry) in assets {
            doc = doc.with_media_asset_entry(id, entry);
        }
        doc
    }

    #[test]
    fn a_container_with_video_and_audio_is_video() {
        assert_eq!(
            classify(&container_entry(Some(1920), 1, None)),
            MediaBinRowKind::Video
        );
    }

    #[test]
    fn an_audio_only_container_is_audio() {
        assert_eq!(
            classify(&container_entry(None, 2, Some(30.0))),
            MediaBinRowKind::Audio
        );
    }

    #[test]
    fn a_container_without_probed_streams_stays_video() {
        // A failed probe records neither dimensions nor an audio count; the
        // file still imported as a container, so it reads as video.
        assert_eq!(
            classify(&container_entry(None, 0, None)),
            MediaBinRowKind::Video
        );
    }

    #[test]
    fn stills_and_sequences_classify_by_kind() {
        let still = MediaAssetEntry::from_absolute("/media/plate.png");
        assert_eq!(classify(&still), MediaBinRowKind::Still);

        let mut sequence = MediaAssetEntry::from_absolute("/media/seq/f_0001.png");
        sequence.kind = AssetKind::Sequence {
            prefix: "f_".into(),
            suffix: ".png".into(),
            padding: 4,
            start: 1,
            end: 48,
        };
        assert_eq!(classify(&sequence), MediaBinRowKind::Video);
    }

    fn bin_document() -> Document {
        let mut audio = container_entry(None, 1, Some(12.0));
        audio.path =
            ravel_core::composition::AssetPath::Absolute(PathBuf::from("/media/voice.wav"));
        let mut offline = container_entry(Some(1920), 0, Some(2.0));
        offline.path = ravel_core::composition::AssetPath::Relative("./gone.mov".into());
        offline.resolved = None;
        document_with(vec![
            ("clip", container_entry(Some(1920), 1, Some(2.5))),
            ("plate", MediaAssetEntry::from_absolute("/media/plate.png")),
            ("voice", audio),
            ("gone", offline),
        ])
    }

    #[test]
    fn rows_are_sorted_by_name_and_carry_the_entry_facts() {
        let doc = bin_document();
        let rows = MediaBinPanel::new().rows(&doc);
        assert_eq!(
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
            ["clip.mov", "gone.mov", "plate.png", "voice.wav"],
        );
        let clip = &rows[0];
        assert_eq!(clip.asset_id, "clip");
        assert_eq!(clip.kind, MediaBinRowKind::Video);
        assert_eq!(clip.duration, Some(2.5));
        assert!(!clip.offline);
        let gone = &rows[1];
        assert!(gone.offline, "resolved == None marks the row offline");
        assert_eq!(rows[3].kind, MediaBinRowKind::Audio);
    }

    #[test]
    fn the_kind_filter_narrows_the_rows() {
        let doc = bin_document();
        let mut panel = MediaBinPanel::new();

        panel.set_filter(MediaBinFilter::Video);
        assert_eq!(
            panel
                .rows(&doc)
                .iter()
                .map(|row| row.asset_id.as_str())
                .collect::<Vec<_>>(),
            ["clip", "gone"],
        );

        panel.set_filter(MediaBinFilter::Still);
        assert_eq!(
            panel
                .rows(&doc)
                .iter()
                .map(|row| row.asset_id.as_str())
                .collect::<Vec<_>>(),
            ["plate"],
        );

        panel.set_filter(MediaBinFilter::Audio);
        assert_eq!(
            panel
                .rows(&doc)
                .iter()
                .map(|row| row.asset_id.as_str())
                .collect::<Vec<_>>(),
            ["voice"],
        );
    }

    #[test]
    fn the_search_matches_a_case_insensitive_substring_of_the_name() {
        let doc = bin_document();
        let mut panel = MediaBinPanel::new();
        panel.set_query("CLIP");
        assert_eq!(
            panel
                .rows(&doc)
                .iter()
                .map(|row| row.asset_id.as_str())
                .collect::<Vec<_>>(),
            ["clip"],
        );

        panel.set_query("  .mov ");
        assert_eq!(
            panel.rows(&doc).len(),
            2,
            "the query is trimmed before matching"
        );

        panel.set_query("nothing-matches-this");
        assert!(panel.rows(&doc).is_empty());
    }

    #[test]
    fn filter_and_search_compose() {
        let doc = bin_document();
        let mut panel = MediaBinPanel::new();
        panel.set_filter(MediaBinFilter::Video);
        panel.set_query("gone");
        assert_eq!(
            panel
                .rows(&doc)
                .iter()
                .map(|row| row.asset_id.as_str())
                .collect::<Vec<_>>(),
            ["gone"],
        );
    }

    #[test]
    fn an_empty_document_has_no_rows() {
        assert!(MediaBinPanel::new().rows(&Document::default()).is_empty());
    }

    #[test]
    fn the_name_falls_back_to_the_asset_id() {
        let mut entry = container_entry(Some(1920), 0, None);
        entry.path = ravel_core::composition::AssetPath::Variable("${MEDIA}".into());
        let doc = document_with(vec![("my-asset", entry)]);
        let rows = MediaBinPanel::new().rows(&doc);
        assert_eq!(rows[0].name, "my-asset");
    }

    // ----- asset_references -------------------------------------------------

    fn media_network(asset_id: &str) -> Graph {
        let node = Node::new(NodeId::next(), "media")
            .with_output("frame", ravel_core::id::DataTypeId::FRAME_BUFFER);
        let mut node = node;
        node.parameters.push(ravel_core::graph::Parameter {
            key: "asset_id".to_string(),
            value: ParameterValue::String(asset_id.to_string()),
        });
        Graph::new().add_node(node).unwrap()
    }

    fn comp_with_layers(name: &str, layers: Vec<Layer>) -> Composition {
        let mut comp = Composition::new(
            CompId::next(),
            name,
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        );
        for layer in layers {
            comp = comp.add_layer(layer);
        }
        comp
    }

    #[test]
    fn references_cover_media_nodes_and_audio_sources_once_per_layer() {
        let by_node = Layer::new(LayerId::next(), "Node layer", media_network("clip"));
        let mut by_audio = Layer::new(LayerId::next(), "Audio layer", Graph::new());
        by_audio.audio = Some(AudioSource {
            asset_id: "clip".to_string(),
            ..AudioSource::default()
        });
        let mut by_both = Layer::new(LayerId::next(), "Both layer", media_network("clip"));
        by_both.audio = Some(AudioSource {
            asset_id: "clip".to_string(),
            ..AudioSource::default()
        });
        let by_other = Layer::new(LayerId::next(), "Other layer", media_network("other"));
        let comp = comp_with_layers("Comp 1", vec![by_node, by_audio, by_both, by_other]);
        let comp_id = comp.id;
        let layer_ids: Vec<LayerId> = comp.layers.iter().map(|layer| layer.id).collect();
        let mut doc = Document::default();
        doc.compositions.insert(comp_id, Arc::new(comp));

        let references = asset_references(&doc, "clip");
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.layer)
                .collect::<Vec<_>>(),
            layer_ids[..3],
            "the first three layers reference the asset, once each, in stack order"
        );
        assert!(references.iter().all(|reference| reference.comp == comp_id));

        assert!(asset_references(&doc, "unused").is_empty());
    }
}

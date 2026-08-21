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

use ravel_core::composition::{AssetKind, Document, MediaAssetEntry, node_asset_reference};
use ravel_core::graph::Node;
use ravel_core::id::{AssetId, CompId, LayerId};
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
    /// Key of [`Document::media_assets`] — the asset's identity, never shown.
    pub asset_id: AssetId,
    /// Display name: the asset's own editable [`MediaAssetEntry::name`], with
    /// the file name of the persisted path as the fallback.
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
                asset_id: *id,
                name: asset_name(entry),
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

/// The display name of an asset: its own [`MediaAssetEntry::name`], which the
/// import path seeds from the file stem and the MediaBin lets the user edit
/// (`AID-3`). Every label an asset appears under reads this one function, so a
/// rename shows up wherever the asset does.
///
/// The fallback — for an entry whose name was cleared by a hand-edited
/// document, since the rename UI refuses a blank one — is the file name of the
/// persisted path: the persisted form is one string for every variant
/// (`AssetPath` Display), and a leading `${VAR}` component still leaves a
/// usable file name. Never the id: an [`AssetId`] is a number the user has no
/// way to connect to a file, so showing it would be worse than showing
/// nothing.
pub fn asset_name(entry: &MediaAssetEntry) -> String {
    if !entry.name.trim().is_empty() {
        return entry.name.clone();
    }
    let text = entry.path.to_string();
    Path::new(&text)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty() && !name.starts_with("${"))
        .unwrap_or_default()
}

/// `m:ss.t` for a duration in seconds.
///
/// Shared with the Properties media-asset section
/// (`crate::properties::media_asset`) so a row and its inspector never spell
/// the same length two ways.
pub fn format_duration(secs: f64) -> String {
    let minutes = (secs / 60.0).floor() as u64;
    let seconds = secs - minutes as f64 * 60.0;
    format!("{minutes}:{seconds:04.1}")
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
pub fn asset_references(document: &Document, asset_id: AssetId) -> Vec<AssetReference> {
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

/// Whether `node` is a media node referencing `asset_id` (the binding
/// `add_media_layer` writes; `video` is the persisted alias of `media`).
fn node_uses_asset(node: &Node, asset_id: AssetId) -> bool {
    node_asset_reference(node) == Some(asset_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{
        AssetMetadata, AudioSource, Composition, Layer, MEDIA_ASSET_PARAM_KEY,
    };
    use ravel_core::graph::{Graph, ParameterValue};
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

    /// Register each entry under a fresh id, keeping the given display name.
    /// The names are what the rows show and sort by; the ids are what a
    /// reference holds, and the tests below never need to spell them.
    fn document_with(assets: Vec<(&str, MediaAssetEntry)>) -> Document {
        let mut doc = Document::default();
        for (name, mut entry) in assets {
            entry.name = name.to_string();
            doc = doc.with_media_asset_entry(AssetId::next(), entry);
        }
        doc
    }

    /// The display names of the panel's rows, in row order.
    fn row_names(panel: &MediaBinPanel, doc: &Document) -> Vec<String> {
        panel.rows(doc).iter().map(|row| row.name.clone()).collect()
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
            ["clip", "gone", "plate", "voice"],
        );
        let clip = &rows[0];
        assert_eq!(
            doc.get_media_asset(clip.asset_id).map(|e| e.name.as_str()),
            Some("clip"),
            "the row's id is the key of the entry it was built from"
        );
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
        assert_eq!(row_names(&panel, &doc), ["clip", "gone"]);

        panel.set_filter(MediaBinFilter::Still);
        assert_eq!(row_names(&panel, &doc), ["plate"]);

        panel.set_filter(MediaBinFilter::Audio);
        assert_eq!(row_names(&panel, &doc), ["voice"]);
    }

    #[test]
    fn the_search_matches_a_case_insensitive_substring_of_the_name() {
        let doc = bin_document();
        let mut panel = MediaBinPanel::new();
        panel.set_query("CLIP");
        assert_eq!(row_names(&panel, &doc), ["clip"]);

        // `gone` and `voice`, so the match is a substring and not a prefix.
        panel.set_query("  o ");
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
        assert_eq!(row_names(&panel, &doc), ["gone"]);
    }

    #[test]
    fn an_empty_document_has_no_rows() {
        assert!(MediaBinPanel::new().rows(&Document::default()).is_empty());
    }

    /// The row shows the asset's own editable name and not the file name of
    /// its path: a rename in the MediaBin is what makes a list of
    /// `clip_0001_v3.mov` readable (`AID-3`).
    #[test]
    fn the_row_shows_the_editable_name_over_the_file_name() {
        let doc = document_with(vec![(
            "Background plate",
            MediaAssetEntry::from_absolute("/media/clip_0001_v3.mov"),
        )]);
        assert_eq!(
            MediaBinPanel::new().rows(&doc)[0].name,
            "Background plate",
            "the row is labelled by the name, not by the path"
        );
    }

    /// A nameless asset can only come from a hand-edited document — the rename
    /// UI refuses a blank name — and falls back to the file name of the path,
    /// never to the id, which the user cannot connect to a file.
    #[test]
    fn a_nameless_asset_falls_back_to_the_file_name() {
        let doc = document_with(vec![(
            "  ",
            MediaAssetEntry::from_absolute("/media/clip.mov"),
        )]);
        assert_eq!(MediaBinPanel::new().rows(&doc)[0].name, "clip.mov");
    }

    // ----- asset_references -------------------------------------------------

    fn media_network(asset_id: AssetId) -> Graph {
        let node = Node::new(NodeId::next(), "media")
            .with_output("frame", ravel_core::id::DataTypeId::FRAME_BUFFER);
        let mut node = node;
        node.parameters.push(ravel_core::graph::Parameter {
            key: MEDIA_ASSET_PARAM_KEY.to_string(),
            value: ParameterValue::String(asset_id.to_param_value()),
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
        let clip = AssetId::next();
        let other = AssetId::next();
        let by_node = Layer::new(LayerId::next(), "Node layer", media_network(clip));
        let mut by_audio = Layer::new(LayerId::next(), "Audio layer", Graph::new());
        by_audio.audio = Some(AudioSource {
            asset_id: clip,
            ..AudioSource::default()
        });
        let mut by_both = Layer::new(LayerId::next(), "Both layer", media_network(clip));
        by_both.audio = Some(AudioSource {
            asset_id: clip,
            ..AudioSource::default()
        });
        let by_other = Layer::new(LayerId::next(), "Other layer", media_network(other));
        let comp = comp_with_layers("Comp 1", vec![by_node, by_audio, by_both, by_other]);
        let comp_id = comp.id;
        let layer_ids: Vec<LayerId> = comp.layers.iter().map(|layer| layer.id).collect();
        let mut doc = Document::default();
        doc.compositions.insert(comp_id, Arc::new(comp));

        let references = asset_references(&doc, clip);
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.layer)
                .collect::<Vec<_>>(),
            layer_ids[..3],
            "the first three layers reference the asset, once each, in stack order"
        );
        assert!(references.iter().all(|reference| reference.comp == comp_id));

        assert!(asset_references(&doc, AssetId::next()).is_empty());
    }
}

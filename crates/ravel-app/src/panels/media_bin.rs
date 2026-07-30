// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! MediaBin panel: the project's media asset list (REQ-UI-008, media-import
//! plan unit 4).
//!
//! Rows come pre-flattened from [`ravel_ui::panels::media_bin`], so rendering
//! is a straight walk over a list — no probing, decoding, or graph walking
//! inside `render()`. Clicking writes the shared `MediaSelection` global and
//! publishes a `PropertiesTarget::MediaAsset` (the same split as the
//! Outliner's layer selection). Thumbnails come from the unit-5
//! [`ThumbnailCache`]: requests are kicked outside `render()`, ready PNGs are
//! decoded into `RenderImage`s when the cache notifies, and anything not
//! ready falls back to the kind icon.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon, Sizable as _, WindowExt as _};
use ravel_core::composition::{AssetKind, MediaAssetEntry};
use ravel_core::runtime::InvalidationHint;
use ravel_i18n::t;
use ravel_ui::document::CompositionSettings;
use ravel_ui::panel::PanelKind;
use ravel_ui::panels::media_bin::{
    AssetReference, MediaBinFilter, MediaBinPanel, MediaBinRow, MediaBinRowKind, asset_references,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;

use crate::assets::RavelIcon;
use crate::media::import::ProbedAsset;
use crate::media::thumbnail::{ThumbnailCache, ThumbnailSource, ThumbnailState};
use crate::project_state::ProjectState;

const HEADER_HEIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 28.0;
const THUMB_WIDTH: f32 = 40.0;
const THUMB_HEIGHT: f32 = 24.0;

pub struct MediaBinGpuiPanel {
    state: MediaBinPanel,
    /// The app-wide document state; `None` only when the panel outlives it.
    project: Option<Entity<ProjectState>>,
    audio: Option<Entity<crate::audio::AudioService>>,
    /// The filtered rows, rebuilt from the document whenever it or the
    /// filter/search state changes (never inside `render()`).
    rows: Vec<MediaBinRow>,
    /// Unit-5 thumbnail cache. Requests are kicked from `rebuild_rows`;
    /// completion notifies, and the observer decodes what is ready.
    thumbnails: Entity<ThumbnailCache>,
    /// Decoded thumbnails by asset id, filled by `refresh_thumbnails`.
    thumb_images: HashMap<String, Arc<RenderImage>>,
    search: Entity<InputState>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    #[allow(dead_code)]
    audio_sub: Option<Subscription>,
    /// Gate for the observer above (see [`super::MirrorEpoch`]).
    mirror_epoch: super::MirrorEpoch,
    #[allow(dead_code)]
    selection_sub: Subscription,
    #[allow(dead_code)]
    thumbnails_sub: Subscription,
    #[allow(dead_code)]
    search_sub: Subscription,
}

impl MediaBinGpuiPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, |this: &mut Self, project, cx| {
                // The asset list changes on import and undo, not on the stream
                // of notifications a drag or a save produces.
                if !this.mirror_epoch.advanced(project.read(cx).mirror_epoch()) {
                    return;
                }
                this.rebuild_rows(cx);
            })
        });
        let audio = cx
            .try_global::<crate::audio::AudioServiceHandle>()
            .and_then(|handle| handle.0.upgrade());
        let audio_sub = audio.as_ref().map(|audio| {
            cx.observe(audio, |_this: &mut Self, _audio, cx| {
                cx.notify();
            })
        });

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        // Selection highlighting only: the rows themselves do not change.
        let selection_sub = cx.observe_global::<super::MediaSelection>(|_this, cx| cx.notify());

        let thumbnails = cx.new(|_| ThumbnailCache::global());
        let thumbnails_sub = cx.observe(&thumbnails, |this: &mut Self, _cache, cx| {
            this.refresh_thumbnails(cx);
            cx.notify();
        });

        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(SharedString::from(t!("media_bin.search")))
        });
        let search_sub = cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, state, event: &InputEvent, _window, cx| {
                if let InputEvent::Change = event {
                    let query = state.read(cx).value().to_string();
                    this.state.set_query(query);
                    this.rebuild_rows(cx);
                }
            },
        );

        let focus_handle = cx.focus_handle();
        let focus_subscriptions =
            super::track_panel_focus(PanelKind::MediaBin, &focus_handle, window, cx);

        let mut panel = Self {
            state: MediaBinPanel::new(),
            project,
            audio,
            rows: Vec::new(),
            thumbnails,
            thumb_images: HashMap::new(),
            search,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            project_sub,
            audio_sub,
            mirror_epoch: super::MirrorEpoch::default(),
            selection_sub,
            thumbnails_sub,
            search_sub,
        };
        panel.rebuild_rows(cx);
        panel
    }

    /// The currently visible rows (tests and the debug inspector).
    pub fn rows(&self) -> &[MediaBinRow] {
        &self.rows
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        self.rows = match &self.project {
            Some(project) => self.state.rows(project.read(cx).document()),
            None => Vec::new(),
        };
        // Kick thumbnail generation for the visible rows here (not in
        // `render()`): `get_or_request` is a cheap in-memory lookup that
        // spawns background work on a miss.
        let entries: Vec<(String, MediaAssetEntry)> = self
            .rows
            .iter()
            .filter_map(|row| {
                let entry = self
                    .project
                    .as_ref()?
                    .read(cx)
                    .document()
                    .media_assets
                    .get(&row.asset_id)?
                    .clone();
                Some((row.asset_id.clone(), entry))
            })
            .collect();
        self.thumbnails.update(cx, |cache, cx| {
            for (_id, entry) in &entries {
                if let Some(path) = &entry.resolved {
                    cache.get_or_request(path, thumbnail_source(&entry.kind), cx);
                }
            }
        });
        self.refresh_thumbnails(cx);
        cx.notify();
    }

    /// Decode whatever the cache has ready into renderable images. Runs on
    /// cache notifications and rebuilds — never in `render()`.
    fn refresh_thumbnails(&mut self, cx: &mut Context<Self>) {
        let entries: Vec<(String, MediaAssetEntry)> = self
            .rows
            .iter()
            .filter(|row| !self.thumb_images.contains_key(&row.asset_id))
            .filter_map(|row| {
                let entry = self
                    .project
                    .as_ref()?
                    .read(cx)
                    .document()
                    .media_assets
                    .get(&row.asset_id)?
                    .clone();
                Some((row.asset_id.clone(), entry))
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        let ready: Vec<(String, Arc<[u8]>)> = self.thumbnails.update(cx, |cache, cx| {
            entries
                .iter()
                .filter_map(|(id, entry)| {
                    let path = entry.resolved.as_ref()?;
                    match cache.get_or_request(path, thumbnail_source(&entry.kind), cx) {
                        ThumbnailState::Ready(bytes) => Some((id.clone(), bytes)),
                        ThumbnailState::Pending | ThumbnailState::Unavailable => None,
                    }
                })
                .collect()
        });
        for (id, bytes) in ready {
            if let Some(image) = decode_thumbnail(&bytes) {
                self.thumb_images.insert(id, image);
            }
        }
        // Assets can leave the document (delete, undo): drop their images so
        // a re-imported id cannot inherit a stale frame — the cache keys by
        // path and regenerates, but the id mapping must not outlive the asset.
        if let Some(project) = &self.project {
            let document = project.read(cx).document();
            self.thumb_images
                .retain(|id, _| document.media_assets.contains_key(id));
        }
    }

    // ----- row interaction --------------------------------------------------

    fn set_filter(&mut self, filter: MediaBinFilter, cx: &mut Context<Self>) {
        self.state.set_filter(filter);
        self.rebuild_rows(cx);
    }

    /// Click semantics: a plain click selects just the row, the platform
    /// modifier toggles it in the selection; a double click additionally adds
    /// the asset to the active composition as a layer. Selection lives in the
    /// shared `MediaSelection` global and is published to Properties by the
    /// same writer.
    fn on_row_click(
        &mut self,
        index: usize,
        click_count: usize,
        toggle: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        let mut assets = super::media_selection(cx).assets().to_vec();
        if toggle {
            if let Some(position) = assets.iter().position(|id| id == &row.asset_id) {
                assets.remove(position);
            } else {
                assets.push(row.asset_id.clone());
            }
        } else {
            assets = vec![row.asset_id.clone()];
        }
        super::set_media_selection(assets, cx);
        if click_count >= 2 {
            add_asset_as_layer(&row.asset_id, cx);
        }
        cx.notify();
    }

    /// Select for an operation aimed at the row under the cursor (right
    /// click): a selection that already holds the row is kept, so the menu
    /// does not throw the rest of the selection away (the Outliner's rule).
    fn select_row_for_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        if !super::media_selection(cx).contains(&row.asset_id) {
            super::set_media_selection(vec![row.asset_id], cx);
        }
        cx.notify();
    }

    // ----- rendering --------------------------------------------------------

    fn filter_button(
        &self,
        filter: MediaBinFilter,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let active = self.state.filter() == filter;
        div()
            .id(SharedString::from(format!("media-bin-filter-{label}")))
            .h(px(18.0))
            .px_1p5()
            .flex()
            .items_center()
            .rounded_sm()
            .cursor_pointer()
            .text_xs()
            .text_color(if active {
                colors.foreground
            } else {
                colors.muted_foreground
            })
            .when(active, |button| button.bg(colors.list_active))
            .hover(|style| style.bg(colors.list_hover))
            .child(t!(label))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.set_filter(filter, cx);
            }))
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        div()
            .flex()
            .items_center()
            .gap_1()
            .h(px(HEADER_HEIGHT))
            .px_1()
            .border_b_1()
            .border_color(colors.border)
            .child(self.filter_button(MediaBinFilter::All, "media_bin.filter.all", cx))
            .child(self.filter_button(MediaBinFilter::Video, "media_bin.filter.video", cx))
            .child(self.filter_button(MediaBinFilter::Still, "media_bin.filter.still", cx))
            .child(self.filter_button(MediaBinFilter::Audio, "media_bin.filter.audio", cx))
            .child(
                div()
                    .flex_grow()
                    .min_w(px(60.0))
                    .child(Input::new(&self.search).xsmall()),
            )
    }

    fn kind_icon(kind: MediaBinRowKind) -> Icon {
        match kind {
            MediaBinRowKind::Video => Icon::new(RavelIcon::MediaBin),
            MediaBinRowKind::Still => Icon::new(RavelIcon::MediaStill),
            MediaBinRowKind::Audio => Icon::new(RavelIcon::Waveform),
        }
    }

    fn render_row(&self, index: usize, row: &MediaBinRow, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let selected = super::media_selection(cx).contains(&row.asset_id);
        let can_layer = !row.offline && super::active_composition(cx).is_some();
        let preparing = self
            .audio
            .as_ref()
            .is_some_and(|audio| audio.read(cx).is_asset_preparing(&row.asset_id));
        let asset_id = row.asset_id.clone();

        let mut content = div()
            .id(SharedString::from(format!("media-bin-row-{index}")))
            .h(px(ROW_HEIGHT))
            // Shrink-proof so a long list overflows the scroll container
            // instead of being squashed into the panel height.
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .text_xs()
            .text_color(if row.offline {
                colors.muted_foreground
            } else {
                colors.foreground
            })
            .when(selected, |row| row.bg(colors.list_active))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    this.on_row_click(index, event.click_count, event.modifiers.platform, cx);
                }),
            );

        // Thumbnail: the decoded frame when the cache produced one, the kind
        // icon while it is pending and when none is available.
        let mut thumb = div()
            .w(px(THUMB_WIDTH))
            .h(px(THUMB_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .border_1()
            .border_color(colors.border)
            .bg(colors.muted)
            .overflow_hidden();
        thumb = match self.thumb_images.get(&row.asset_id) {
            Some(image) => thumb.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover)),
            None => thumb.child(
                Self::kind_icon(row.kind)
                    .size_3p5()
                    .text_color(colors.muted_foreground),
            ),
        };
        content = content.child(thumb);

        content = content.child(
            // A file name is one line: `min_w_0` allows the shrink that
            // `truncate` needs, so the name ellipsizes instead of wrapping and
            // the trailing duration/offline badges keep their place.
            div()
                .flex_grow()
                .min_w_0()
                .truncate()
                .child(SharedString::from(row.name.clone())),
        );

        if row.offline {
            content = content.child(
                div()
                    .flex_shrink_0()
                    .text_color(colors.danger)
                    .child(t!("media_bin.offline")),
            );
        }
        if preparing {
            content = content.child(
                div()
                    .flex_shrink_0()
                    .text_color(colors.info)
                    .child(t!("audio.preparing")),
            );
        }
        if let Some(duration) = row.duration {
            content = content.child(
                div()
                    .flex_shrink_0()
                    .text_color(colors.muted_foreground)
                    .child(format_duration(duration)),
            );
        }

        let entity = cx.entity().downgrade();
        let offline = row.offline;
        content
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _event: &MouseDownEvent, _window, cx| {
                    this.select_row_for_menu(index, cx);
                }),
            )
            .context_menu(move |menu, _window, _cx| {
                let layer_entity = entity.clone();
                let comp_entity = entity.clone();
                let delete_entity = entity.clone();
                let layer_asset = asset_id.clone();
                let comp_asset = asset_id.clone();
                let delete_asset = asset_id.clone();
                menu.item(
                    PopupMenuItem::new(t!("media_bin.menu.add_as_layer"))
                        .disabled(!can_layer)
                        .on_click(move |_, _window, cx| {
                            let _ = layer_entity.update(cx, |_this, cx| {
                                add_asset_as_layer(&layer_asset, cx);
                            });
                        }),
                )
                .item(
                    PopupMenuItem::new(t!("media_bin.menu.new_composition"))
                        .disabled(offline)
                        .on_click(move |_, _window, cx| {
                            let _ = comp_entity.update(cx, |_this, cx| {
                                new_composition_from_asset(&comp_asset, cx);
                            });
                        }),
                )
                .item(PopupMenuItem::new(t!("media_bin.menu.delete")).on_click(
                    move |_, window, cx| {
                        let _ = delete_entity.update(cx, |_this, cx| {
                            request_delete_asset(&delete_asset, window, cx);
                        });
                    },
                ))
            })
            .into_any_element()
    }
}

/// The decode path a thumbnail request takes for the asset kind.
fn thumbnail_source(kind: &AssetKind) -> ThumbnailSource {
    match kind {
        AssetKind::Container => ThumbnailSource::Container,
        AssetKind::Still => ThumbnailSource::Still,
        AssetKind::Sequence { .. } => ThumbnailSource::Sequence,
    }
}

/// Decode a cached thumbnail PNG into the straight-alpha BGRA
/// [`RenderImage`] the `img` element consumes (the same layout
/// `viewer::frame_buffer_to_render_image` produces).
fn decode_thumbnail(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let mut pixels = image::load_from_memory(bytes).ok()?.into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(
        image::Frame::new(pixels),
        1,
    ))))
}

/// `m:ss.t` for a row's duration column.
fn format_duration(secs: f64) -> String {
    let minutes = (secs / 60.0).floor() as u64;
    let seconds = secs - minutes as f64 * 60.0;
    format!("{minutes}:{seconds:04.1}")
}

/// The asset the delete/add operations resolve, or `None` when it is gone.
fn asset_entry(asset_id: &str, cx: &App) -> Option<(Entity<ProjectState>, MediaAssetEntry)> {
    let project = cx
        .try_global::<crate::project_state::ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())?;
    let entry = project
        .read(cx)
        .document()
        .media_assets
        .get(asset_id)?
        .clone();
    Some((project, entry))
}

/// Add the asset to the active composition as a layer at the playhead, by
/// feeding it back through the unit-3 import path (`ProjectState::import_media`
/// dedupes on the resolved path, so the existing asset is reused and only the
/// layer is created — one undo step). A no-op for offline assets and without
/// an active composition.
pub fn add_asset_as_layer(asset_id: &str, cx: &mut App) {
    let Some((project, entry)) = asset_entry(asset_id, cx) else {
        return;
    };
    let Some(path) = entry.resolved.clone() else {
        return;
    };
    if super::active_composition(cx).is_none() {
        return;
    }
    project.update(cx, |project, cx| {
        project.import_media(
            vec![ProbedAsset {
                path,
                kind: entry.kind.clone(),
                metadata: entry.metadata.clone(),
            }],
            vec![],
            cx,
        );
    });
}

/// Create a composition matching the asset's resolution, frame rate, and
/// length (decision 5: never retrofit an existing composition), make it
/// active, and place the asset as its one layer. Missing metadata falls back
/// to the project defaults. A no-op for offline assets (their layer could
/// not resolve).
pub fn new_composition_from_asset(asset_id: &str, cx: &mut App) {
    let Some((project, entry)) = asset_entry(asset_id, cx) else {
        return;
    };
    if entry.resolved.is_none() {
        return;
    }
    let metadata = &entry.metadata;
    let frame_rate = metadata
        .frame_rate
        .unwrap_or(CompositionSettings::FALLBACK_FRAME_RATE);
    let settings = CompositionSettings {
        name: composition_name_for(asset_id, &entry),
        resolution: match (metadata.width, metadata.height) {
            (Some(width), Some(height)) => (width, height),
            _ => CompositionSettings::FALLBACK_RESOLUTION,
        },
        frame_rate,
        duration_frames: metadata
            .duration_secs
            .map(|secs| (secs * frame_rate.as_f64()).ceil().max(1.0) as u64)
            .unwrap_or(CompositionSettings::FALLBACK_DURATION),
        background_color: ravel_core::types::Color::BLACK,
    };
    project.update(cx, |project, cx| {
        project.create_composition(settings, cx);
    });
    // The new composition is active now; the layer lands on it through the
    // same import path as "add as layer".
    add_asset_as_layer(asset_id, cx);
}

/// The new composition's name: the asset's file stem, uniquified against the
/// document by `create_composition`'s caller side (the Outliner renames).
fn composition_name_for(asset_id: &str, entry: &MediaAssetEntry) -> String {
    let text = entry.path.to_string();
    std::path::Path::new(&text)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty() && !stem.starts_with("${"))
        .unwrap_or_else(|| asset_id.to_string())
}

/// The confirmation message for deleting an in-use asset: the localized lead
/// plus one line per referencing composition (`Comp: layer, layer`). `None`
/// when nothing references the asset — the caller deletes without asking.
pub fn delete_confirmation(
    document: &ravel_core::composition::Document,
    asset_id: &str,
) -> Option<String> {
    let references = asset_references(document, asset_id);
    if references.is_empty() {
        return None;
    }
    let mut message = format!("{} ({})", t!("media_bin.delete.in_use"), references.len());
    let mut comps: Vec<(ravel_core::id::CompId, Vec<ravel_core::id::LayerId>)> = Vec::new();
    for AssetReference { comp, layer } in references {
        match comps.iter_mut().find(|(id, _)| *id == comp) {
            Some((_, layers)) => layers.push(layer),
            None => comps.push((comp, vec![layer])),
        }
    }
    for (comp, layers) in comps {
        let comp_name = document
            .get_composition(comp)
            .map(|comp| comp.name.clone())
            .unwrap_or_default();
        let layer_names: Vec<String> = layers
            .iter()
            .filter_map(|layer| {
                document
                    .get_composition(comp)
                    .and_then(|comp| comp.get_layer(*layer))
                    .map(|layer| layer.name.clone())
            })
            .collect();
        message.push_str(&format!("\n{comp_name}: {}", layer_names.join(", ")));
    }
    Some(message)
}

/// Delete the asset from the project, asking first when layers still use it
/// (the confirmation names the referencing compositions and layers, like the
/// composition delete guard). One undo step; the selection and Properties
/// target are pruned by `ProjectState`'s document-change hook.
pub fn request_delete_asset(asset_id: &str, window: &mut Window, cx: &mut App) {
    let Some(project) = cx
        .try_global::<crate::project_state::ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        return;
    };
    let Some(message) = delete_confirmation(project.read(cx).document(), asset_id) else {
        delete_asset(asset_id, cx);
        return;
    };

    let asset_id = asset_id.to_string();
    window.open_alert_dialog(cx, move |alert, _window, _cx| {
        let asset_id = asset_id.clone();
        alert
            .confirm()
            .title(SharedString::from(t!("media_bin.delete.title")))
            .description(SharedString::from(message.clone()))
            .show_cancel(true)
            .on_ok(move |_event, _window, cx| {
                delete_asset(&asset_id, cx);
                true
            })
    });
}

/// The document edit behind asset deletion: drop the entry and commit.
/// Layers that referenced it go offline (the `media` node renders a
/// transparent frame, decision 7) rather than failing evaluation.
fn delete_asset(asset_id: &str, cx: &mut App) {
    let Some(project) = cx
        .try_global::<crate::project_state::ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        return;
    };
    project.update(cx, |project, cx| {
        let mut doc = project.document().clone();
        if doc.media_assets.remove(asset_id).is_some() {
            project.commit_document(doc, InvalidationHint::Structural, cx);
        }
    });
}

impl Panel for MediaBinGpuiPanel {
    fn panel_name(&self) -> &'static str {
        PanelKind::MediaBin.panel_id()
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = super::is_panel_focused(PanelKind::MediaBin, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        super::tab_title(
            Some(PanelKind::MediaBin),
            SharedString::from(t!("panel.media_bin")),
            color,
        )
    }
}

impl EventEmitter<PanelEvent> for MediaBinGpuiPanel {}

impl Focusable for MediaBinGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MediaBinGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let mut list = div()
            .id("media-bin-list")
            .debug_selector(|| "media-bin-panel".into())
            .flex_grow()
            .flex()
            .flex_col()
            .overflow_y_scroll();

        // No assets yet (or nothing passes the filter/search): the import
        // paths — File ▸ Import, OS file drop — are the way out.
        if self.rows.is_empty() {
            list = list.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .child(SharedString::from(t!("media_bin.empty"))),
            );
        } else {
            let rows = self.rows.clone();
            for (index, row) in rows.iter().enumerate() {
                list = list.child(self.render_row(index, row, cx));
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(colors.border)
            .bg(colors.list)
            .track_focus(&self.focus_handle)
            .child(self.render_header(cx))
            .child(list)
    }
}

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
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon, Sizable as _, WindowExt as _};
use ravel_core::color::ColorSpace;
use ravel_core::composition::{AssetKind, MediaAssetEntry, MediaAssets};
use ravel_core::id::AssetId;
use ravel_core::runtime::InvalidationHint;
use ravel_i18n::t;
use ravel_ui::document::CompositionSettings;
use ravel_ui::panels::media_bin::{
    AssetReference, MediaBinFilter, MediaBinPanel, MediaBinRow, MediaBinRowKind, asset_references,
    format_duration,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::assets::RavelIcon;
use crate::media::thumbnail::{ThumbnailCache, ThumbnailSource, ThumbnailState};
use crate::project_state::ProjectState;

const HEADER_HEIGHT: f32 = 24.0;
const ROW_HEIGHT: f32 = 28.0;
const THUMB_WIDTH: f32 = 40.0;
const THUMB_HEIGHT: f32 = 24.0;

/// Inline rename of a MediaBin row. The subscription commits the edited name
/// on Enter or blur and is dropped with the rename (the Outliner's layer
/// rename, same shape).
struct AssetRename {
    asset: AssetId,
    input: Entity<InputState>,
    #[allow(dead_code)]
    sub: Subscription,
}

pub struct MediaBinGpuiPanel {
    state: MediaBinPanel,
    /// The app-wide document state; `None` only when the panel outlives it.
    project: Option<Entity<ProjectState>>,
    audio: Option<Entity<crate::audio::AudioService>>,
    /// The filtered rows, rebuilt from the document whenever it or the
    /// filter/search state changes (never inside `render()`).
    rows: Vec<MediaBinRow>,
    /// The persistent media-asset map the rows were built from. Layer edits
    /// mint a new `Document` while sharing this map, so `ptr_eq` against it
    /// skips the row walk altogether.
    ///
    /// The map rather than the whole `Document`: holding the document would
    /// pin the previous snapshot's compositions and layer graphs alive for as
    /// long as the panel is open, to answer a question about one field.
    last_media_assets: Option<MediaAssets>,
    /// Unit-5 thumbnail cache. Requests are kicked from `rebuild_rows`;
    /// completion notifies, and the observer decodes what is ready.
    thumbnails: Entity<ThumbnailCache>,
    /// Decoded thumbnails by asset id, filled by `refresh_thumbnails`. The
    /// identity records what the image was generated from: same id but a new
    /// path, decode source, or resolved input colour space means the stored
    /// image is stale and must be regenerated.
    thumb_images: HashMap<AssetId, (ThumbnailIdentity, Arc<RenderImage>)>,
    search: Entity<InputState>,
    /// In-flight inline rename, `None` when no row is being renamed.
    rename: Option<AssetRename>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    project_sub: Option<Subscription>,
    #[allow(dead_code)]
    audio_sub: Option<Subscription>,
    /// Gate for the observer above (see [`super::MirrorEpoch`]).
    mirror_epoch: super::MirrorEpoch,
    #[allow(dead_code)]
    selection_sub: Subscription,
    /// Pays off the rebuild skipped while the panel was behind another tab
    /// (see [`super::on_became_visible`]).
    #[allow(dead_code)]
    visibility_sub: Subscription,
    #[allow(dead_code)]
    thumbnails_sub: Subscription,
    #[allow(dead_code)]
    search_sub: Subscription,
}

impl MediaBinGpuiPanel {
    pub fn new(
        instance: ravel_ui::layout::PanelInstanceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade());
        let project_sub = project.as_ref().map(|project| {
            cx.observe(project, move |this: &mut Self, project, cx| {
                // Behind another tab the rows have no reader, so the rebuild
                // waits for the panel to come back — before the epoch gate, so
                // the skipped import stays owed (`visibility_sub` below).
                if !super::is_instance_visible(instance, cx) {
                    return;
                }
                // The asset list changes on import and undo, not on the stream
                // of notifications a drag or a save produces.
                if !this.mirror_epoch.advanced(project.read(cx).mirror_epoch()) {
                    return;
                }
                this.rebuild_rows_if_media_changed(cx);
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

        // Selection highlighting only: the rows themselves do not change.
        let selection_sub = cx.observe_global::<super::MediaSelection>(|_this, cx| cx.notify());

        // The cache location is a preference (`SET-8`), read here because here
        // is where the cache is built — a location changed while the panel is
        // up applies the next time it is, which in practice is the next
        // launch. The row's description says so.
        let root = crate::app_settings::cache_root(cx);
        let thumbnails = cx.new(|_| ThumbnailCache::new(root));
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

        // Coming back into view rebuilds the rows if the media map moved while
        // the panel was away. `last_media_assets` is the record of what the
        // rows were built from, so the check that decides it is the same one
        // the observer above uses.
        let visibility_sub = super::on_became_visible(instance, cx, |this, cx| {
            if let Some(project) = this.project.clone() {
                let epoch = project.read(cx).mirror_epoch();
                this.mirror_epoch.advanced(epoch);
            }
            this.rebuild_rows_if_media_changed(cx);
        });

        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(instance, &focus_handle, window, cx);

        let mut panel = Self {
            state: MediaBinPanel::new(),
            project,
            audio,
            rows: Vec::new(),
            last_media_assets: None,
            thumbnails,
            thumb_images: HashMap::new(),
            search,
            rename: None,
            focus_handle,
            focus_subscriptions,
            project_sub,
            audio_sub,
            mirror_epoch: super::MirrorEpoch::default(),
            visibility_sub,
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
        super::sync_probe::record(super::sync_probe::PanelSync::MediaBinRows);
        let document = self
            .project
            .as_ref()
            .map(|project| project.read(cx).document().clone());
        let rows = document
            .as_ref()
            .map(|document| self.state.rows(document))
            .unwrap_or_default();
        let rows_changed = self.rows != rows;
        self.rows = rows;
        self.last_media_assets = document
            .as_ref()
            .map(|document| document.media_assets.clone());
        // Kick thumbnail generation for the visible rows here (not in
        // `render()`): `get_or_request` is a cheap in-memory lookup that
        // spawns background work on a miss.
        let entries: Vec<(AssetId, MediaAssetEntry)> = self
            .rows
            .iter()
            .filter_map(|row| {
                let entry = document.as_ref()?.media_assets.get(&row.asset_id)?.clone();
                Some((row.asset_id, entry))
            })
            .collect();
        self.thumbnails.update(cx, |cache, cx| {
            for (_id, entry) in &entries {
                if let Some(path) = &entry.resolved {
                    cache.get_or_request(
                        path,
                        thumbnail_source(&entry.kind),
                        entry.input_color_space().0,
                        cx,
                    );
                }
            }
        });
        let thumbnails_changed = self.refresh_thumbnails(cx);
        // An inline rename whose asset left the document (deleted, undone) has
        // no row to render into: drop it instead of keeping an invisible
        // editor whose blur would name an asset that is no longer there. It
        // has to happen even when the rows did not move, so it gets its own
        // notify reason.
        let mut dropped_rename = false;
        if let Some(rename) = &self.rename {
            let alive = document
                .as_ref()
                .is_some_and(|document| document.media_assets.contains_key(&rename.asset));
            if !alive {
                self.rename = None;
                dropped_rename = true;
            }
        }
        if rows_changed || thumbnails_changed || dropped_rename {
            cx.notify();
        }
    }

    /// The document observer only needs to rebuild when the persistent media
    /// map changed. Layer edits share that map, so this check avoids building
    /// the same row model on every drag tick.
    fn rebuild_rows_if_media_changed(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let document = project.read(cx).document();
        if self
            .last_media_assets
            .as_ref()
            .is_some_and(|last| last.ptr_eq(&document.media_assets))
        {
            return;
        }
        self.rebuild_rows(cx);
    }

    /// Decode whatever the cache has ready into renderable images. Runs on
    /// cache notifications and rebuilds — never in `render()`.
    fn refresh_thumbnails(&mut self, cx: &mut Context<Self>) -> bool {
        let entries: Vec<(AssetId, ThumbnailIdentity)> = self
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
                let identity = thumbnail_identity(&entry)?;
                // A stored image is fresh only if it was generated from this
                // exact identity. An input-colour-space change keeps the same
                // id and path, so keying on the id alone would show the old
                // space's thumbnail forever.
                if self
                    .thumb_images
                    .get(&row.asset_id)
                    .is_some_and(|(stored, _)| *stored == identity)
                {
                    return None;
                }
                Some((row.asset_id, identity))
            })
            .collect();
        if entries.is_empty() {
            return false;
        }
        let ready: Vec<(AssetId, ThumbnailIdentity, Arc<[u8]>)> =
            self.thumbnails.update(cx, |cache, cx| {
                entries
                    .iter()
                    .filter_map(|(id, identity)| {
                        match cache.get_or_request(
                            &identity.path,
                            identity.source,
                            identity.input_color_space,
                            cx,
                        ) {
                            ThumbnailState::Ready(bytes) => Some((*id, identity.clone(), bytes)),
                            ThumbnailState::Pending | ThumbnailState::Unavailable => None,
                        }
                    })
                    .collect()
            });
        let mut changed = false;
        for (id, identity, bytes) in ready {
            if let Some(image) = decode_thumbnail(&bytes) {
                self.thumb_images.insert(id, (identity, image));
                changed = true;
            }
        }
        // Assets can leave the document (delete, undo): drop their images so
        // the map does not grow without bound. Since `.ravprj` v9 a
        // re-imported file takes a fresh `AssetId`, so it could not inherit a
        // stale frame even if the entry stayed.
        if let Some(project) = &self.project {
            let document = project.read(cx).document();
            let before = self.thumb_images.len();
            self.thumb_images
                .retain(|id, _| document.media_assets.contains_key(id));
            changed |= before != self.thumb_images.len();
        }
        changed
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
            if let Some(position) = assets.iter().position(|id| *id == row.asset_id) {
                assets.remove(position);
            } else {
                assets.push(row.asset_id);
            }
        } else {
            assets = vec![row.asset_id];
        }
        super::set_media_selection(assets, cx);
        if click_count >= 2 {
            add_asset_as_layer(row.asset_id, cx);
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
        if !super::media_selection(cx).contains(row.asset_id) {
            super::set_media_selection(vec![row.asset_id], cx);
        }
        cx.notify();
    }

    /// Start renaming a row in place. The caller focuses the input — a panel
    /// never grabs focus on its own (`.agents/rules/gpui.md`).
    fn begin_rename(
        &mut self,
        asset: AssetId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        let name = self
            .rows
            .iter()
            .find(|row| row.asset_id == asset)?
            .name
            .clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, state, event: &InputEvent, _window, cx| match event {
                // Enter and blur both commit: leaving the field is the same
                // intent as confirming it (the Outliner's rule).
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    let name = state.read(cx).value().to_string();
                    this.commit_rename(name, cx);
                }
                _ => {}
            },
        );
        self.rename = Some(AssetRename {
            asset,
            input: input.clone(),
            sub,
        });
        cx.notify();
        Some(input)
    }

    /// Apply an edited asset name as **one** undo step. A blank or unchanged
    /// name just closes the editor: a blank name falls back to the file name
    /// of the path, which would leave the row showing something the user did
    /// not type.
    ///
    /// Two assets are allowed to share a name — nothing references an asset by
    /// it since `.ravprj` v9 — so no numbering is imposed on what the user
    /// typed.
    fn commit_rename(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        cx.notify();
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(project) = self.project.clone() else {
            return;
        };
        project.update(cx, |project, cx| {
            let Some(mut entry) = project.document().get_media_asset(rename.asset).cloned() else {
                return;
            };
            if entry.name == name {
                return;
            }
            entry.name = name;
            // A name is a label: no evaluation, no compiled chain and no
            // decode depends on it.
            let document = project
                .document()
                .clone()
                .with_media_asset_entry(rename.asset, entry);
            project.commit_document(document, InvalidationHint::None, cx);
        });
    }

    /// Abandon an inline rename, keeping the asset's current name.
    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            cx.notify();
        }
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
        let selected = super::media_selection(cx).contains(row.asset_id);
        let can_layer = !row.offline && super::active_composition(cx).is_some();
        let preparing = self
            .audio
            .as_ref()
            .is_some_and(|audio| audio.read(cx).is_asset_preparing(row.asset_id));
        let asset_id = row.asset_id;

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

        // Drag onto the Timeline or the Viewer to make a layer of it. An
        // offline asset has nothing to decode, so it does not drag at all
        // (the same reason its menu item is disabled).
        if can_layer {
            content = content.on_drag(
                DraggedAsset {
                    asset_id: row.asset_id,
                    name: row.name.clone(),
                },
                |drag, _offset, _window, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                },
            );
        }

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
            Some((_, image)) => {
                thumb.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
            }
            None => thumb.child(
                Self::kind_icon(row.kind)
                    .size_3p5()
                    .text_color(colors.muted_foreground),
            ),
        };
        content = content.child(thumb);

        let renaming = match &self.rename {
            Some(rename) if rename.asset == row.asset_id => Some(rename.input.clone()),
            _ => None,
        };
        content = match renaming {
            Some(input) => {
                // Raw key handling, the approved exception for text entry
                // (`.agents/rules/gpui.md`): `InputState` emits no event for
                // Escape and its Enter action does not reach a subscriber
                // here, so the row confirms and cancels the edit itself. Blur
                // still commits.
                let commit_input = input.clone();
                content
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _window, cx| {
                        match event.keystroke.key.as_str() {
                            // GPUI names the key "enter"; "return" is accepted
                            // too so a platform reporting the physical key
                            // name still confirms.
                            "enter" | "return" => {
                                let name = commit_input.read(cx).value().to_string();
                                this.commit_rename(name, cx);
                            }
                            "escape" => this.cancel_rename(cx),
                            _ => {}
                        }
                    }))
                    .child(div().flex_grow().child(Input::new(&input).xsmall()))
            }
            None => content.child(
                // A file name is one line: `min_w_0` allows the shrink that
                // `truncate` needs, so the name ellipsizes instead of wrapping
                // and the trailing duration/offline badges keep their place.
                div()
                    .flex_grow()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(row.name.clone())),
            ),
        };

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
                let rename_entity = entity.clone();
                let delete_entity = entity.clone();
                let layer_asset = asset_id;
                let comp_asset = asset_id;
                let rename_asset = asset_id;
                let delete_asset = asset_id;
                menu.item(
                    PopupMenuItem::new(t!("media_bin.menu.add_as_layer"))
                        .disabled(!can_layer)
                        .on_click(move |_, _window, cx| {
                            let _ = layer_entity.update(cx, |_this, cx| {
                                add_asset_as_layer(layer_asset, cx);
                            });
                        }),
                )
                .item(
                    PopupMenuItem::new(t!("media_bin.menu.new_composition"))
                        .disabled(offline)
                        .on_click(move |_, _window, cx| {
                            let _ = comp_entity.update(cx, |_this, cx| {
                                new_composition_from_asset(comp_asset, cx);
                            });
                        }),
                )
                .item(
                    // Renaming an offline asset is exactly how a project full
                    // of moved footage gets readable again, so this one is
                    // never disabled.
                    PopupMenuItem::new(t!("media_bin.menu.rename")).on_click(
                        move |_, window, cx| {
                            let _ = rename_entity.update(cx, |this, cx| {
                                // Focus belongs to the click, not to the
                                // panel's own construction.
                                if let Some(input) = this.begin_rename(rename_asset, window, cx) {
                                    input.update(cx, |state, cx| state.focus(window, cx));
                                }
                            });
                        },
                    ),
                )
                .item(PopupMenuItem::new(t!("media_bin.menu.delete")).on_click(
                    move |_, window, cx| {
                        let _ = delete_entity.update(cx, |_this, cx| {
                            request_delete_asset(delete_asset, window, cx);
                        });
                    },
                ))
            })
            .into_any_element()
    }
}

/// Payload of a MediaBin → Timeline / Viewer asset drag, and its own drag
/// preview (the pattern `DragScrub` and `DragCurvePoint` use).
///
/// It names **only the pressed row**: the payload is baked when that row
/// renders, which is before the press has updated `MediaSelection`. The drop
/// side expands it against the live selection instead
/// ([`dropped_asset_ids`]), so a multi-selection travels as a whole and a row
/// outside the selection travels alone — the rule the context menu already
/// follows.
#[derive(Clone)]
pub struct DraggedAsset {
    pub asset_id: AssetId,
    /// Display name, for the preview that follows the pointer.
    pub name: String,
}

impl Render for DraggedAsset {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(colors.border)
            .bg(colors.popover)
            .text_xs()
            .text_color(colors.popover_foreground)
            .child(SharedString::from(self.name.clone()))
    }
}

/// The assets a drop of `drag` should place: the whole media selection when
/// the dragged row is part of it, otherwise just that row.
pub fn dropped_asset_ids(drag: &DraggedAsset, cx: &App) -> Vec<AssetId> {
    let selection = super::media_selection(cx);
    if selection.contains(drag.asset_id) {
        selection.assets().to_vec()
    } else {
        vec![drag.asset_id]
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

/// What an asset's thumbnail is generated from. The disk cache keys on the
/// same triple; keeping it beside the decoded image lets the panel spot a
/// stale thumbnail when any part changes underneath an unchanged asset id
/// (a relink, or the user setting the input colour space).
#[derive(Clone, Debug, PartialEq)]
struct ThumbnailIdentity {
    path: PathBuf,
    source: ThumbnailSource,
    input_color_space: ColorSpace,
}

/// The identity of `entry`'s thumbnail, or `None` while the asset is
/// offline (no resolved path to decode from).
fn thumbnail_identity(entry: &MediaAssetEntry) -> Option<ThumbnailIdentity> {
    Some(ThumbnailIdentity {
        path: entry.resolved.clone()?,
        source: thumbnail_source(&entry.kind),
        input_color_space: entry.input_color_space().0,
    })
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

/// The asset the delete/add operations resolve, or `None` when it is gone.
fn asset_entry(asset_id: AssetId, cx: &App) -> Option<(Entity<ProjectState>, MediaAssetEntry)> {
    let project = cx
        .try_global::<crate::project_state::ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())?;
    let entry = project
        .read(cx)
        .document()
        .get_media_asset(asset_id)?
        .clone();
    Some((project, entry))
}

/// Add the asset to the active composition as a layer at the playhead — one
/// undo step. A no-op for offline assets (nothing would resolve) and without
/// an active composition.
pub fn add_asset_as_layer(asset_id: AssetId, cx: &mut App) {
    add_assets_as_layers(&[asset_id], ProjectState::playhead_frame(cx), cx);
}

/// [`add_asset_as_layer`] for a whole set at a chosen frame — the drop
/// handlers of the Timeline and the Viewer. One `commit_document` covers the
/// batch, so dropping a multi-selection is a single undo step.
pub fn add_assets_as_layers(asset_ids: &[AssetId], start_frame: i64, cx: &mut App) {
    let Some(project) = cx
        .try_global::<crate::project_state::ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        return;
    };
    if super::active_composition(cx).is_none() {
        return;
    }
    // Offline assets have no file to decode; the menu item is disabled for
    // them and a drag must not smuggle one in.
    let online: Vec<AssetId> = asset_ids
        .iter()
        .filter(|id| {
            project
                .read(cx)
                .document()
                .get_media_asset(**id)
                .is_some_and(|entry| entry.resolved.is_some())
        })
        .copied()
        .collect();
    if online.is_empty() {
        return;
    }
    project.update(cx, |project, cx| {
        project.add_asset_layers(&online, start_frame, cx);
    });
}

/// Create a composition matching the asset's resolution, frame rate, and
/// length (decision 5: never retrofit an existing composition), make it
/// active, and place the asset as its one layer. Missing metadata falls back
/// to the project defaults. A no-op for offline assets (their layer could
/// not resolve).
pub fn new_composition_from_asset(asset_id: AssetId, cx: &mut App) {
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
        name: composition_name_for(&entry),
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
///
/// The asset's own display name is the fallback when the path yields no stem;
/// never the id, which would name the composition after a number.
fn composition_name_for(entry: &MediaAssetEntry) -> String {
    let text = entry.path.to_string();
    std::path::Path::new(&text)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty() && !stem.starts_with("${"))
        .unwrap_or_else(|| entry.name.clone())
}

/// The confirmation message for deleting an in-use asset: the localized lead
/// plus one line per referencing composition (`Comp: layer, layer`). `None`
/// when nothing references the asset — the caller deletes without asking.
pub fn delete_confirmation(
    document: &ravel_core::composition::Document,
    asset_id: AssetId,
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
pub fn request_delete_asset(asset_id: AssetId, window: &mut Window, cx: &mut App) {
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

    window.open_alert_dialog(cx, move |alert, _window, _cx| {
        alert
            .confirm()
            .title(SharedString::from(t!("media_bin.delete.title")))
            .description(SharedString::from(message.clone()))
            .show_cancel(true)
            .on_ok(move |_event, _window, cx| {
                delete_asset(asset_id, cx);
                true
            })
    });
}

/// The document edit behind asset deletion: drop the entry and commit.
/// Layers that referenced it go offline (the `media` node renders a
/// transparent frame, decision 7) rather than failing evaluation.
fn delete_asset(asset_id: AssetId, cx: &mut App) {
    let Some(project) = cx
        .try_global::<crate::project_state::ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        return;
    };
    project.update(cx, |project, cx| {
        let mut doc = project.document().clone();
        if doc.media_assets.remove(&asset_id).is_some() {
            project.commit_document(doc, InvalidationHint::Structural, cx);
        }
    });
}

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

#[cfg(test)]
mod tests {
    // No `use super::*;` here: that glob (re-importing the file's `gpui::*`)
    // combined with the `#[gpui::test]` macro makes rustc 1.95.0 crash with
    // SIGBUS. Import explicitly instead.
    use super::{
        InvalidationHint, MediaBinGpuiPanel, ProjectState, ThumbnailCache, asset_references,
    };
    use crate::media::import::ProbedAsset;
    use crate::media::thumbnail::ThumbnailGenerator;
    use crate::project_state::ProjectStateHandle;
    use gpui::{AppContext as _, ParentElement as _, Pixels, Size, Styled as _, px};
    use ravel_core::color::ColorSpace;
    use ravel_core::composition::{AssetKind, AssetMetadata, AudioStreamMetadata};
    use ravel_core::types::{FrameBuffer, FrameRate};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    const WINDOW_SIZE: Size<Pixels> = Size {
        width: px(800.0),
        height: px(600.0),
    };

    fn probed_clip(path: &str) -> ProbedAsset {
        ProbedAsset {
            path: PathBuf::from(path),
            kind: AssetKind::Container,
            metadata: AssetMetadata {
                width: Some(1920),
                height: Some(1080),
                frame_rate: Some(FrameRate::new(24, 1)),
                duration_secs: Some(2.0),
                codec: Some("fake".into()),
                color_space: None,
                audio_stream_count: 1,
                audio_streams: vec![AudioStreamMetadata {
                    stream_index: 1,
                    codec: Some("fake-audio".into()),
                    sample_rate: 48_000,
                    channels: 2,
                }],
                file_size: 100,
            },
        }
    }

    /// The globals a MediaBin panel reads, plus a fresh project registered as
    /// the app's document state.
    fn init(cx: &mut gpui::TestAppContext) -> gpui::Entity<ProjectState> {
        crate::project_state::disable_background_eval_for_tests();
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(super::super::FocusedPanelGlobal(None));
            cx.set_global(super::super::SelectedPropertiesTarget::default());
            cx.set_global(super::super::MediaSelection::default());
            cx.set_global(super::super::PlaybackPosition::default());
            let project = cx.new(ProjectState::new);
            cx.set_global(ProjectStateHandle(project.downgrade()));
            project
        })
    }

    /// Renaming a row commits the trimmed name as **one** undo step, and a
    /// blank name is not an edit at all. The name is a label: the asset keeps
    /// its id, so the layer that references it is untouched (`AID-3`).
    #[gpui::test]
    fn renaming_an_asset_commits_once_and_ignores_a_blank_name(cx: &mut gpui::TestAppContext) {
        let project = init(cx);
        let window = cx.add_window(|window, cx| {
            MediaBinGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        project.update(cx, |project, cx| {
            project.import_media(vec![probed_clip("/media/clip_0001_v3.mov")], vec![], cx);
        });
        cx.run_until_parked();

        let asset = window
            .read_with(cx, |panel, _| panel.rows[0].asset_id)
            .unwrap();
        // The asset is referenced by a layer, so a rename that touched the
        // identity would show up as a broken reference below.
        cx.update(|cx| super::add_asset_as_layer(asset, cx));
        cx.run_until_parked();
        let references = |cx: &mut gpui::TestAppContext| {
            project.read_with(cx, |project, _| {
                asset_references(project.document(), asset).len()
            })
        };
        assert_eq!(references(cx), 1, "the layer reads the asset");

        let names = |cx: &mut gpui::TestAppContext| {
            window
                .read_with(cx, |panel, _| {
                    panel
                        .rows
                        .iter()
                        .map(|row| row.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap()
        };
        assert_eq!(names(cx), ["clip_0001_v3"]);

        window
            .update(cx, |panel, window, cx| {
                assert!(panel.begin_rename(asset, window, cx).is_some());
                assert!(panel.rename.is_some());
                panel.commit_rename("  Background plate  ".into(), cx);
                assert!(panel.rename.is_none(), "committing closes the editor");
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            names(cx),
            ["Background plate"],
            "the trimmed name reaches the row"
        );
        assert_eq!(references(cx), 1, "and the reference still lands");

        // A blank name closes the editor without touching the document.
        window
            .update(cx, |panel, window, cx| {
                panel.begin_rename(asset, window, cx);
                panel.commit_rename("   ".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            names(cx),
            ["Background plate"],
            "a blank name is not an edit"
        );

        project.update(cx, |project, cx| project.undo(cx));
        cx.run_until_parked();
        assert_eq!(
            names(cx),
            ["clip_0001_v3"],
            "one undo restores the imported name"
        );
    }

    /// An asset that leaves the document while its row is being renamed takes
    /// the editor with it: a blur commit against a missing asset would
    /// otherwise be a no-op nobody can see.
    #[gpui::test]
    fn deleting_the_asset_being_renamed_drops_the_editor(cx: &mut gpui::TestAppContext) {
        let project = init(cx);
        let window = cx.add_window(|window, cx| {
            MediaBinGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
        });
        project.update(cx, |project, cx| {
            project.import_media(vec![probed_clip("/media/clip.mov")], vec![], cx);
        });
        cx.run_until_parked();
        let asset = window
            .read_with(cx, |panel, _| panel.rows[0].asset_id)
            .unwrap();
        window
            .update(cx, |panel, window, cx| {
                panel.begin_rename(asset, window, cx);
            })
            .unwrap();

        cx.update(|cx| super::delete_asset(asset, cx));
        cx.run_until_parked();
        window
            .read_with(cx, |panel, _| assert!(panel.rename.is_none()))
            .unwrap();
    }

    /// A stored thumbnail must not survive a change of the asset's resolved
    /// input colour space: the id and the path are unchanged, so keying the
    /// decoded image on the id alone would show the old space's thumbnail
    /// forever (CodeRabbit review on the MED-APP-32 fix).
    #[gpui::test]
    fn a_colour_space_change_regenerates_the_thumbnail(cx: &mut gpui::TestAppContext) {
        // No `ravel_i18n::init` here: i18n is process-global, and the lib
        // tests share one process — initializing it flips label lookups for
        // concurrently running tests (node_editor's driven-params test reads
        // the type id "constant", not the English display name). Uninitialised
        // `t!` returns the raw key, which this test never asserts on.
        crate::project_state::disable_background_eval_for_tests();
        let project = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(super::super::FocusedPanelGlobal(None));
            cx.set_global(super::super::SelectedPropertiesTarget::default());
            cx.set_global(super::super::MediaSelection::default());
            cx.set_global(super::super::PlaybackPosition::default());
            let project = cx.new(ProjectState::new);
            cx.set_global(ProjectStateHandle(project.downgrade()));
            project
        });

        struct TestRoot {
            panel: gpui::Entity<MediaBinGpuiPanel>,
        }
        impl gpui::Render for TestRoot {
            fn render(
                &mut self,
                _window: &mut gpui::Window,
                _cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                gpui::div().size_full().child(self.panel.clone())
            }
        }

        let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
        let captured_in_window = captured.clone();
        let _window = cx.open_window(WINDOW_SIZE, move |window, cx| {
            let panel = cx
                .new(|cx| MediaBinGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx));
            *captured_in_window.borrow_mut() = Some(panel.clone());
            gpui_component::Root::new(cx.new(|_| TestRoot { panel }), window, cx)
        });
        let panel = captured
            .borrow_mut()
            .take()
            .expect("panel entity should be created");

        // A generator that records the colour space each request carried.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorder = calls.clone();
        let generator: ThumbnailGenerator = Arc::new(move |_path, _source, space| {
            recorder.lock().unwrap().push(space);
            Ok(FrameBuffer::from_f32(8, 8, vec![0.5; 8 * 8 * 4]))
        });
        panel.update(cx, |panel, cx| {
            panel.thumbnails = cx.new(|_| ThumbnailCache::with_generator(None, generator));
        });

        // The thumbnail cache keys by statting the source, so the clip needs
        // a real file behind it.
        let temp = tempfile::tempdir().expect("create temp dir");
        let clip = temp.path().join("clip.mov");
        std::fs::write(&clip, b"media fixture").expect("write media fixture");

        project.update(cx, |project, cx| {
            project.import_media(vec![probed_clip(clip.to_str().unwrap())], vec![], cx);
        });
        cx.run_until_parked();
        // The injected cache is not the entity the panel's observer
        // subscribes to, so pull the ready bytes explicitly — the observer
        // would do the same on the real cache's notification.
        panel.update(cx, |panel, cx| panel.refresh_thumbnails(cx));

        let asset_id = panel.read_with(cx, |panel, _| panel.rows[0].asset_id);
        // Untagged .mov: the extension default resolves to sRGB.
        assert_eq!(
            *calls.lock().unwrap(),
            vec![ColorSpace::SRGB],
            "first thumbnail request"
        );
        panel.read_with(cx, |panel, _| {
            let (identity, _) = panel
                .thumb_images
                .get(&asset_id)
                .expect("thumbnail image stored");
            assert_eq!(identity.input_color_space, ColorSpace::SRGB);
        });

        // The user reinterprets the clip as linear: same id, same path.
        project.update(cx, |project, cx| {
            let mut doc = project.document().clone();
            let entry = doc
                .media_assets
                .get_mut(&asset_id)
                .expect("asset in document");
            entry.color_space = Some(ColorSpace::LINEAR_REC709);
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();
        panel.update(cx, |panel, cx| panel.refresh_thumbnails(cx));

        assert_eq!(
            *calls.lock().unwrap(),
            vec![ColorSpace::SRGB, ColorSpace::LINEAR_REC709],
            "the new space must be decoded, not served from the stale image"
        );
        panel.read_with(cx, |panel, _| {
            let (identity, _) = panel
                .thumb_images
                .get(&asset_id)
                .expect("thumbnail image stored");
            assert_eq!(identity.input_color_space, ColorSpace::LINEAR_REC709);
        });
    }
}

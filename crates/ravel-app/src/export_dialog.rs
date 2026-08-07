// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The export dialog behind `File ▸ Export…` (`render-export-plan.md`,
//! unit 5).
//!
//! The form holds widgets and nothing else: it is read out on OK into a
//! [`ExportSettings`], which resolves into the job description
//! ([`ravel_ui::export`]). Nothing reaches the render queue until the button
//! is pressed, so a cancelled dialog costs nothing.
//!
//! # The format list
//!
//! Built from [`available_encoders`], the runtime enumeration `ravel-cli`'s
//! `list codecs` and `--format` also read — one authority for what this build
//! and this machine can write. Formats that cannot be written are **shown
//! with the reason** rather than hidden: "AV1 is not available" is a fact
//! about the build, and a list that silently omitted it would leave the user
//! looking for a menu entry that never existed.
//!
//! Video targets are listed for the same reason and are never selectable:
//! the container writer is out of scope for this generation of the plan, so
//! even an H.264 encoder the machine really has has nowhere to put its
//! output. That is the same refusal `ravel-cli` reports as `CodecNoWriter`.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputState, NumberInput};
use gpui_component::radio::Radio;
use gpui_component::select::{Select, SelectState};
use gpui_component::{ActiveTheme, Disableable as _, Sizable as _};
use ravel_core::id::CompId;
use ravel_core::media::encode::{
    Availability, EncodeTarget, EncoderAvailability, PngDepth, UnavailableReason,
};
use ravel_core::media::{ImageFormat, VideoCodec};
use ravel_i18n::t;
use ravel_ui::export::{DEFAULT_PADDING, ExportSettings};
use std::path::PathBuf;

/// Width of the label column, matching the composition dialog's.
const LABEL_WIDTH: f32 = 120.0;

/// One row of the dialog's format list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatChoice {
    /// The still-image format this row writes, and the depth it writes at.
    /// `None` for a video target, which is never selectable.
    pub sequence: Option<(ImageFormat, PngDepth)>,
    /// Name of the format. Not translated — these are format names, not
    /// prose.
    pub label: SharedString,
    /// Why the row cannot be picked, already localized. `None` means it can.
    pub unavailable: Option<SharedString>,
}

impl FormatChoice {
    pub fn is_selectable(&self) -> bool {
        self.unavailable.is_none()
    }
}

/// The dialog's format list, in the enumeration's own order.
///
/// Takes the table rather than calling [`available_encoders`] itself, so the
/// mapping can be tested against a machine that is not this one.
pub fn format_choices(encoders: &[EncoderAvailability]) -> Vec<FormatChoice> {
    let mut choices = Vec::new();
    for entry in encoders {
        let unavailable = match &entry.availability {
            Availability::Unavailable(reason) => Some(unavailable_text(reason)),
            Availability::Available(_) => None,
        };
        match entry.target {
            EncodeTarget::ImageSequence(format) => {
                for depth in sequence_depths(format) {
                    // A format the enumeration offers but no writer in the
                    // tree produces (TIFF, DPX) is refused for a different
                    // reason than an absent encoder, and says so.
                    let unavailable = unavailable.clone().or_else(|| {
                        ravel_core::media::encode::SequenceCodec::from_image_format(format, depth)
                            .is_none()
                            .then(|| SharedString::from(t!("export.unavailable.no_writer")))
                    });
                    choices.push(FormatChoice {
                        sequence: Some((format, depth)),
                        label: sequence_label(format, depth),
                        unavailable,
                    });
                }
            }
            EncodeTarget::Video(codec) => choices.push(FormatChoice {
                sequence: None,
                label: SharedString::from(video_label(codec)),
                // Available or not, there is nowhere to put the frames yet.
                unavailable: Some(
                    unavailable
                        .clone()
                        .unwrap_or_else(|| SharedString::from(t!("export.unavailable.no_writer"))),
                ),
            }),
        }
    }
    choices
}

/// Depths a still-image format is offered at: PNG has two, everything else
/// one row.
fn sequence_depths(format: ImageFormat) -> Vec<PngDepth> {
    match format {
        ImageFormat::Png => vec![PngDepth::Eight, PngDepth::Sixteen],
        _ => vec![PngDepth::Eight],
    }
}

fn sequence_label(format: ImageFormat, depth: PngDepth) -> SharedString {
    match format {
        ImageFormat::Png => SharedString::from(format!("PNG {}-bit", depth.bits())),
        ImageFormat::Exr => SharedString::from("OpenEXR"),
        ImageFormat::Tiff => SharedString::from("TIFF"),
        ImageFormat::Dpx => SharedString::from("DPX"),
    }
}

fn video_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::H265 => "H.265",
        VideoCodec::Av1 => "AV1",
        VideoCodec::ProRes => "ProRes",
        VideoCodec::DnxHr => "DNxHR",
        VideoCodec::Vp8 => "VP8",
        VideoCodec::Vp9 => "VP9",
    }
}

/// A sentence for a refusal the probe reports.
///
/// [`UnavailableReason`] carries no prose by design — it is a classification,
/// and this is the one place it becomes something a user reads.
fn unavailable_text(reason: &UnavailableReason) -> SharedString {
    match reason {
        UnavailableReason::FfmpegNotLinked => {
            SharedString::from(t!("export.unavailable.ffmpeg_not_linked"))
        }
        UnavailableReason::FfmpegEncoderMissing { candidates } => SharedString::from(format!(
            "{} ({})",
            t!("export.unavailable.ffmpeg_encoder_missing"),
            candidates.join(", ")
        )),
        UnavailableReason::PlatformApiUnavailable { api } => {
            SharedString::from(format!("{} ({api})", t!("export.unavailable.platform_api")))
        }
        UnavailableReason::NoPlatformRouteOnThisOs => {
            SharedString::from(t!("export.unavailable.no_platform_route"))
        }
        UnavailableReason::NotOffered => SharedString::from(t!("export.unavailable.not_offered")),
    }
}

/// The export dialog's body.
pub struct ExportForm {
    /// Compositions of the open document, in the order the picker lists them.
    comps: Vec<(CompId, String)>,
    composition: Entity<SelectState<Vec<SharedString>>>,
    start: Entity<InputState>,
    end: Entity<InputState>,
    choices: Vec<FormatChoice>,
    /// Index into `choices`; always a selectable row.
    format: usize,
    directory: Entity<InputState>,
    prefix: Entity<InputState>,
    suffix: Entity<InputState>,
    padding: Entity<InputState>,
    overwrite: bool,
    audio: bool,
    /// Whether a soundtrack is possible at all: this build can decode, and
    /// the chosen composition has audio layers.
    audio_possible: bool,
    /// The refusal shown under the form, set by the OK button when the
    /// settings do not resolve.
    error: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl ExportForm {
    /// Build the form for `document`'s compositions, opening on `active`.
    ///
    /// `initial` supplies the field values; `choices` the format list. Both
    /// are passed in rather than derived here so the caller (the workspace)
    /// keeps the document access and this stays a widget holder.
    pub fn new(
        comps: Vec<(CompId, String)>,
        initial: ExportSettings,
        choices: Vec<FormatChoice>,
        audio_possible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let labels: Vec<SharedString> = comps
            .iter()
            .map(|(_, name)| SharedString::from(name.clone()))
            .collect();
        let selected = comps
            .iter()
            .position(|(id, _)| Some(*id) == initial.comp)
            .unwrap_or(0);
        let composition = cx.new(|cx| {
            SelectState::new(
                labels,
                Some(gpui_component::IndexPath::default().row(selected)),
                window,
                cx,
            )
        });
        let text = |value: &str, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        // The first row that can actually be written; the list always holds
        // one (the PNG writer needs no FFmpeg).
        let format = choices
            .iter()
            .position(FormatChoice::is_selectable)
            .unwrap_or(0);
        // Focus stays with the dialog's own focus trap: a form must not grab
        // focus while it is being constructed (`.agents/rules/gpui.md`).
        Self {
            start: text(&initial.start, window, cx),
            end: text(&initial.end, window, cx),
            directory: text(&initial.directory, window, cx),
            prefix: text(&initial.prefix, window, cx),
            suffix: text(&initial.suffix, window, cx),
            padding: text(&initial.padding, window, cx),
            overwrite: initial.overwrite,
            audio: initial.audio && audio_possible,
            audio_possible,
            comps,
            composition,
            choices,
            format,
            error: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Read the widgets back out. Called by the OK button.
    pub fn settings(&self, cx: &App) -> ExportSettings {
        let comp = self
            .composition
            .read(cx)
            .selected_index(cx)
            .and_then(|index| self.comps.get(index.row))
            .or_else(|| self.comps.first())
            .map(|(id, _)| *id);
        let (format, png_depth) = self
            .choices
            .get(self.format)
            .and_then(|choice| choice.sequence)
            .unwrap_or((ImageFormat::Png, PngDepth::Eight));
        ExportSettings {
            comp,
            start: self.start.read(cx).value().to_string(),
            end: self.end.read(cx).value().to_string(),
            format,
            png_depth,
            directory: self.directory.read(cx).value().to_string(),
            prefix: self.prefix.read(cx).value().to_string(),
            suffix: self.suffix.read(cx).value().to_string(),
            padding: self.padding.read(cx).value().to_string(),
            overwrite: self.overwrite,
            audio: self.audio && self.audio_possible,
        }
    }

    /// Name of the composition the form is pointing at, for the queue row.
    pub fn composition_name(&self, cx: &App) -> String {
        self.composition
            .read(cx)
            .selected_index(cx)
            .and_then(|index| self.comps.get(index.row))
            .or_else(|| self.comps.first())
            .map(|(_, name)| name.clone())
            .unwrap_or_default()
    }

    /// Show a refusal under the form, keeping the dialog open.
    pub fn show_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    /// Ask the platform for an output directory and write it into the field.
    fn browse(&mut self, _event: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let directory = self.directory.downgrade();
        cx.spawn_in(_window, async move |_this, cx| {
            match receiver.await {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        let text = path.to_string_lossy().into_owned();
                        let _ = directory.update_in(cx, |state, window, cx| {
                            state.set_value(text, window, cx);
                        });
                    }
                }
                // Cancelled, or the app is shutting down.
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(error)) => tracing::error!(%error, "the output directory dialog failed"),
            }
        })
        .detach();
    }

    fn row(&self, label: String, control: AnyElement, cx: &App) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .py(px(3.0))
            .child(
                div()
                    .w(px(LABEL_WIDTH))
                    .flex_shrink_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(SharedString::from(label)),
            )
            .child(div().flex_grow().child(control))
    }

    /// The format list: one radio per row, unavailable rows disabled with the
    /// reason beside them.
    fn format_list(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().colors.muted_foreground;
        let mut list = div().flex().flex_col().gap(px(2.0));
        for (index, choice) in self.choices.iter().enumerate() {
            let selectable = choice.is_selectable();
            let radio = Radio::new(("export-format", index))
                .label(choice.label.clone())
                .checked(index == self.format)
                .disabled(!selectable)
                .when(selectable, |radio| {
                    radio.on_click(cx.listener(move |this, _checked, _window, cx| {
                        if this.format != index {
                            this.format = index;
                            this.error = None;
                            cx.notify();
                        }
                    }))
                });
            list =
                list.child(div().flex().items_center().gap_2().child(radio).when_some(
                    choice.unavailable.clone(),
                    |row, reason| {
                        row.child(div().text_xs().truncate().text_color(muted).child(reason))
                    },
                ));
        }
        list
    }
}

impl Focusable for ExportForm {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ExportForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().colors.muted_foreground;
        let range = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_grow()
                    .child(NumberInput::new(&self.start).small()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from("–")),
            )
            .child(div().flex_grow().child(NumberInput::new(&self.end).small()));

        let directory = div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().flex_grow().child(Input::new(&self.directory).small()))
            .child(
                gpui_component::button::Button::new("export-browse")
                    .small()
                    .label(SharedString::from(t!("export.browse")))
                    .on_click(cx.listener(Self::browse)),
            );

        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1()
            .track_focus(&self.focus_handle)
            .child(
                self.row(
                    t!("export.field.composition"),
                    Select::new(&self.composition)
                        .small()
                        .w_full()
                        .into_any_element(),
                    cx,
                ),
            )
            .child(self.row(t!("export.field.range"), range.into_any_element(), cx))
            .child(self.row(
                t!("export.field.format"),
                self.format_list(cx).into_any_element(),
                cx,
            ))
            .child(self.row(
                t!("export.field.directory"),
                directory.into_any_element(),
                cx,
            ))
            .child(self.row(
                t!("export.field.prefix"),
                Input::new(&self.prefix).small().into_any_element(),
                cx,
            ))
            .child(self.row(
                t!("export.field.suffix"),
                Input::new(&self.suffix).small().into_any_element(),
                cx,
            ))
            .child(self.row(
                t!("export.field.padding"),
                NumberInput::new(&self.padding).small().into_any_element(),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pt_1()
                    .child(
                        Checkbox::new("export-overwrite")
                            .label(SharedString::from(t!("export.field.overwrite")))
                            .checked(self.overwrite)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.overwrite = *checked;
                                this.error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("export-audio")
                            .label(SharedString::from(t!("export.field.audio")))
                            .checked(self.audio && self.audio_possible)
                            .disabled(!self.audio_possible)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.audio = *checked;
                                cx.notify();
                            })),
                    )
                    .when(!self.audio_possible, |column| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(t!("export.field.audio_hint"))),
                        )
                    }),
            )
            .when_some(self.error.clone(), |form, message| {
                form.child(
                    div()
                        .pt_1()
                        .text_xs()
                        .text_color(cx.theme().colors.danger)
                        .child(message),
                )
            })
    }
}

/// The directory an export opens on: beside the saved project, or the user's
/// home when the project has never been saved.
pub fn default_output_directory(project_path: Option<&std::path::Path>) -> PathBuf {
    project_path
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The settings an export dialog opens with for `comp`.
pub fn initial_settings(
    comp: CompId,
    name: &str,
    duration: u64,
    project_path: Option<&std::path::Path>,
) -> ExportSettings {
    let mut settings = ExportSettings::for_composition(
        comp,
        name,
        duration,
        default_output_directory(project_path),
    );
    settings.padding = DEFAULT_PADDING.to_string();
    settings
}

// A `use super::*;` glob in a test module in a file that expands the gpui
// proc macros crashes rustc 1.95 (SIGBUS); name what the tests need instead —
// the same constraint `panels/mod.rs` records.
#[cfg(test)]
mod format_tests {
    use super::{FormatChoice, format_choices};
    use gpui::SharedString;
    use ravel_core::media::encode::{
        Availability, EncodeRoute, EncodeTarget, EncoderAvailability, PlatformApi, PngDepth,
        UnavailableReason,
    };
    use ravel_core::media::{ImageFormat, VideoCodec};

    fn sequence(format: ImageFormat, availability: Availability) -> EncoderAvailability {
        EncoderAvailability {
            target: EncodeTarget::ImageSequence(format),
            availability,
        }
    }

    fn video(codec: VideoCodec, availability: Availability) -> EncoderAvailability {
        EncoderAvailability {
            target: EncodeTarget::Video(codec),
            availability,
        }
    }

    fn locales() {
        // The reasons are `t!` lookups; without a catalog they fall back to
        // the key itself, which is still a distinct string per reason.
        let _ = ravel_i18n::translate("export.unavailable.no_writer");
    }

    #[test]
    fn png_is_offered_at_both_depths_and_exr_once() {
        locales();
        let choices = format_choices(&[
            sequence(
                ImageFormat::Png,
                Availability::Available(EncodeRoute::Native),
            ),
            sequence(
                ImageFormat::Exr,
                Availability::Available(EncodeRoute::Native),
            ),
        ]);
        assert_eq!(choices.len(), 3);
        assert_eq!(
            choices[0].sequence,
            Some((ImageFormat::Png, PngDepth::Eight))
        );
        assert_eq!(
            choices[1].sequence,
            Some((ImageFormat::Png, PngDepth::Sixteen))
        );
        assert_eq!(
            choices[2].sequence,
            Some((ImageFormat::Exr, PngDepth::Eight))
        );
        assert!(choices.iter().all(FormatChoice::is_selectable));
    }

    /// The unit's requirement: an unavailable format is shown **with its
    /// reason**, not dropped from the list.
    #[test]
    fn an_unavailable_format_stays_in_the_list_with_a_reason() {
        locales();
        let choices = format_choices(&[
            sequence(
                ImageFormat::Png,
                Availability::Available(EncodeRoute::Native),
            ),
            video(
                VideoCodec::Av1,
                Availability::Unavailable(UnavailableReason::FfmpegNotLinked),
            ),
            video(
                VideoCodec::H264,
                Availability::Unavailable(UnavailableReason::PlatformApiUnavailable {
                    api: PlatformApi::Vaapi,
                }),
            ),
        ]);
        assert_eq!(choices.len(), 4, "nothing was hidden");
        let av1 = &choices[2];
        assert_eq!(av1.label, SharedString::from("AV1"));
        assert!(!av1.is_selectable());
        assert!(av1.unavailable.is_some());
        let h264 = &choices[3];
        assert!(
            h264.unavailable
                .as_ref()
                .is_some_and(|text| text.contains("VA-API") || text.contains("vaapi")),
            "the reason names the API that is missing: {:?}",
            h264.unavailable,
        );
    }

    /// A video encoder the machine really has is still not selectable: the
    /// container writer is out of scope, which is the same refusal
    /// `ravel-cli` reports as `CodecNoWriter`.
    #[test]
    fn an_available_video_codec_is_still_not_selectable() {
        locales();
        let choices = format_choices(&[video(
            VideoCodec::H264,
            Availability::Available(EncodeRoute::Platform {
                api: PlatformApi::VideoToolbox,
                encoder: "h264_videotoolbox",
            }),
        )]);
        assert_eq!(choices.len(), 1);
        assert!(!choices[0].is_selectable());
        assert!(choices[0].unavailable.is_some());
    }

    /// A still-image format with no writer in the tree is refused for its own
    /// reason rather than being offered and failing on OK.
    #[test]
    fn a_sequence_format_with_no_writer_is_not_selectable() {
        locales();
        let choices = format_choices(&[sequence(
            ImageFormat::Tiff,
            Availability::Available(EncodeRoute::Native),
        )]);
        assert_eq!(choices.len(), 1);
        assert!(!choices[0].is_selectable());
    }

    /// The list this machine actually produces always offers something: the
    /// PNG writer needs no FFmpeg, so an export is possible in every build.
    #[test]
    fn the_real_encoder_table_always_offers_a_writable_format() {
        locales();
        let choices = format_choices(&ravel_media::encode::available_encoders());
        assert!(
            choices.iter().any(FormatChoice::is_selectable),
            "every build must be able to export something",
        );
        assert!(
            choices.iter().any(|choice| !choice.is_selectable()),
            "the video targets are listed with their reasons",
        );
    }
}

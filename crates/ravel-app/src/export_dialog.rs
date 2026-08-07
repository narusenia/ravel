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

/// One composition the dialog's picker offers.
///
/// Carries whether that composition has sound, because the dialog outlives
/// the composition it opened on: the picker can be moved to another one, and
/// an answer computed once at opening time would then describe a composition
/// the user is no longer exporting. That is exactly how a project with a
/// soundtrack was rendered silent without a word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompChoice {
    pub id: CompId,
    pub name: String,
    /// Whether any layer of this composition carries audio
    /// (`export::composition_has_audio`).
    pub has_audio: bool,
}

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
    comps: Vec<CompChoice>,
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
    /// What the user asked for, **ungated**. Whether it is possible depends
    /// on the composition currently picked, which is a different question and
    /// is asked at read time ([`audio_possible`](Self::audio_possible)):
    /// folding the two together here is what made "switch to a composition
    /// with sound" leave the box permanently off.
    audio: bool,
    /// Whether this build can decode an audio asset at all. A fact about the
    /// build, so it never changes while the dialog is open.
    decode_available: bool,
    /// The refusal shown under the form, set by the OK button when the
    /// settings do not resolve.
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    /// Redraws the form when the picker's selection changes, which is what
    /// re-asks [`audio_possible`](Self::audio_possible) for the composition
    /// now chosen. Held for the form's lifetime, as GPUI subscriptions must
    /// be.
    _composition_sub: Subscription,
}

impl ExportForm {
    /// Build the form for `document`'s compositions, opening on `active`.
    ///
    /// `initial` supplies the field values; `choices` the format list. Both
    /// are passed in rather than derived here so the caller (the workspace)
    /// keeps the document access and this stays a widget holder.
    ///
    /// `decode_available` is the build's ability to decode audio at all
    /// (`export::AUDIO_DECODE_AVAILABLE`); whether a *particular* composition
    /// has sound rides on its [`CompChoice`].
    pub fn new(
        comps: Vec<CompChoice>,
        initial: ExportSettings,
        choices: Vec<FormatChoice>,
        decode_available: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let labels: Vec<SharedString> = comps
            .iter()
            .map(|comp| SharedString::from(comp.name.clone()))
            .collect();
        let selected = comps
            .iter()
            .position(|comp| Some(comp.id) == initial.comp)
            .unwrap_or(0);
        let composition = cx.new(|cx| {
            SelectState::new(
                labels,
                Some(gpui_component::IndexPath::default().row(selected)),
                window,
                cx,
            )
        });
        // The picker notifies when a row is confirmed; nothing else tells the
        // form that the composition — and with it whether a soundtrack is
        // possible — has changed.
        let composition_sub = cx.observe(&composition, |_this, _picker, cx| cx.notify());
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
            audio: initial.audio,
            decode_available,
            comps,
            composition,
            choices,
            format,
            error: None,
            focus_handle: cx.focus_handle(),
            _composition_sub: composition_sub,
        }
    }

    /// The composition the picker currently points at.
    fn selected_comp(&self, cx: &App) -> Option<&CompChoice> {
        self.composition
            .read(cx)
            .selected_index(cx)
            .and_then(|index| self.comps.get(index.row))
            .or_else(|| self.comps.first())
    }

    /// Whether this export could carry a soundtrack: the build can decode,
    /// and the composition **currently** picked has audio layers.
    ///
    /// Asked afresh every time rather than stored, because the picker moves.
    pub fn audio_possible(&self, cx: &App) -> bool {
        self.decode_available && self.selected_comp(cx).is_some_and(|comp| comp.has_audio)
    }

    /// Read the widgets back out. Called by the OK button.
    pub fn settings(&self, cx: &App) -> ExportSettings {
        let comp = self.selected_comp(cx).map(|comp| comp.id);
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
            audio: self.audio && self.audio_possible(cx),
        }
    }

    /// Name of the composition the form is pointing at, for the queue row.
    pub fn composition_name(&self, cx: &App) -> String {
        self.selected_comp(cx)
            .map(|comp| comp.name.clone())
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
        // Re-asked on every draw, and the picker's `cx.notify()` is what makes
        // a draw happen when the composition changes.
        let audio_possible = self.audio_possible(cx);
        // Two different facts read the same way otherwise: a build that cannot
        // decode anything, and a composition that has nothing to decode. The
        // build comes first — it is true of every composition in the list.
        let audio_hint_key = if !self.decode_available {
            Some("export.field.audio_no_decoder")
        } else if !audio_possible {
            Some("export.field.audio_silent_composition")
        } else {
            None
        };
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
                            .checked(self.audio && audio_possible)
                            .disabled(!audio_possible)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.audio = *checked;
                                cx.notify();
                            })),
                    )
                    .when_some(audio_hint_key, |column, key| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(t!(key))),
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
mod form_tests {
    use super::{CompChoice, ExportForm, format_choices, initial_settings};
    use ravel_core::id::CompId;
    use ravel_core::media::ImageFormat;
    use ravel_ui::export::{ExportError, ExportSettings};

    fn comp() -> CompId {
        CompId::new(3)
    }

    fn initial() -> ExportSettings {
        let mut settings = initial_settings(comp(), "shot 010", 120, None);
        settings.directory = "/tmp/ravel-export-form-test".to_owned();
        settings
    }

    fn choice(id: CompId, name: &str, has_audio: bool) -> CompChoice {
        CompChoice {
            id,
            name: name.to_owned(),
            has_audio,
        }
    }

    fn form(window: &mut gpui::Window, cx: &mut gpui::Context<ExportForm>) -> ExportForm {
        ExportForm::new(
            vec![
                choice(comp(), "shot 010", true),
                choice(CompId::new(4), "b", true),
            ],
            initial(),
            format_choices(&ravel_media::encode::available_encoders()),
            true,
            window,
            cx,
        )
    }

    /// The form hands back what it was opened with, so a dialog that is
    /// confirmed untouched exports exactly what it offered.
    #[gpui::test]
    fn the_form_returns_the_settings_it_opened_with(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(form);
        window
            .update(cx, |form, _window, cx| {
                let settings = form.settings(cx);
                assert_eq!(settings, initial());
                assert_eq!(form.composition_name(cx), "shot 010");
                // The row the form opens on is one that can actually be
                // written, whatever this machine's encoder table looks like.
                settings.resolve().expect("the opening form is exportable");
            })
            .unwrap();
    }

    /// The dialog's own half of the "out before in" criterion: what the
    /// widgets hold resolves to the same refusal the headless form gives.
    #[gpui::test]
    fn an_inverted_range_typed_into_the_form_is_refused(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(form);
        window
            .update(cx, |form, window, cx| {
                form.start
                    .update(cx, |state, cx| state.set_value("100", window, cx));
                form.end
                    .update(cx, |state, cx| state.set_value("99", window, cx));
                assert_eq!(form.settings(cx).resolve(), Err(ExportError::EmptyRange));

                // And the other way round, so the assertion above is about
                // the order rather than about the field being read at all.
                form.end
                    .update(cx, |state, cx| state.set_value("120", window, cx));
                let request = form.settings(cx).resolve().expect("a forward range");
                assert_eq!(request.range, 100..121);
            })
            .unwrap();
    }

    /// Typed output fields reach the resolved job description.
    #[gpui::test]
    fn typed_output_fields_reach_the_job_description(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(form);
        window
            .update(cx, |form, window, cx| {
                form.directory
                    .update(cx, |state, cx| state.set_value("/tmp/renders", window, cx));
                form.prefix
                    .update(cx, |state, cx| state.set_value("beauty_", window, cx));
                form.suffix
                    .update(cx, |state, cx| state.set_value("_v2", window, cx));
                form.padding
                    .update(cx, |state, cx| state.set_value("6", window, cx));
                form.overwrite = true;

                let settings = form.settings(cx);
                assert_eq!(settings.format, ImageFormat::Png);
                let request = settings.resolve().expect("resolves");
                assert_eq!(
                    request.output.frame_path(42),
                    std::path::PathBuf::from("/tmp/renders/beauty_000042_v2.png"),
                );
                assert_eq!(
                    request.overwrite,
                    ravel_core::runtime::OverwritePolicy::Replace,
                );
            })
            .unwrap();
    }

    /// The form draws.
    ///
    /// One of the few things `.agents/rules/gpui.md` keeps GPUI tests for —
    /// behaviour that depends on actual rendering. A dialog whose `render`
    /// panics (a duplicated element id, a widget built without its state) is
    /// invisible to every other test here, and the first thing to find it
    /// would otherwise be the user opening `File ▸ Export…`.
    #[gpui::test]
    fn the_form_renders(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(form);
        let visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(600.0)));
        cx.run_until_parked();
    }

    /// A build that cannot decode audio, or a composition without any, must
    /// not produce a request that asks for a soundtrack.
    #[gpui::test]
    fn a_render_with_no_possible_soundtrack_does_not_ask_for_one(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            ExportForm::new(
                vec![choice(comp(), "shot 010", true)],
                initial(),
                format_choices(&ravel_media::encode::available_encoders()),
                // A build that cannot decode: the composition's own sound is
                // beside the point.
                false,
                window,
                cx,
            )
        });
        window
            .update(cx, |form, _window, cx| {
                assert!(!form.audio_possible(cx));
                assert!(!form.settings(cx).audio);
                assert!(!form.settings(cx).resolve().expect("resolves").audio);
            })
            .unwrap();
    }

    /// The dialog outlives the composition it opened on. Picking one that has
    /// sound must re-enable the soundtrack — the bug this pins is a project
    /// with audio being exported silently because the *first* composition had
    /// none.
    #[gpui::test]
    fn picking_a_composition_with_sound_re_enables_the_soundtrack(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let with_sound = CompId::new(4);
        let window = cx.add_window(|window, cx| {
            ExportForm::new(
                vec![
                    choice(comp(), "silent", false),
                    choice(with_sound, "voiced", true),
                ],
                initial(),
                format_choices(&ravel_media::encode::available_encoders()),
                true,
                window,
                cx,
            )
        });
        window
            .update(cx, |form, _window, cx| {
                assert!(!form.audio_possible(cx), "the form opens on the silent one");
                assert!(!form.settings(cx).audio);
            })
            .unwrap();

        window
            .update(cx, |form, window, cx| {
                form.composition.update(cx, |picker, cx| {
                    picker.set_selected_index(
                        Some(gpui_component::IndexPath::default().row(1)),
                        window,
                        cx,
                    );
                });
            })
            .unwrap();
        cx.run_until_parked();

        window
            .update(cx, |form, _window, cx| {
                assert_eq!(form.composition_name(cx), "voiced");
                assert!(
                    form.audio_possible(cx),
                    "the checkbox follows the picker, not the composition the dialog opened on",
                );
                let settings = form.settings(cx);
                assert!(settings.comp == Some(with_sound));
                assert!(
                    settings.audio,
                    "the soundtrack is on by default and was never turned off by hand",
                );
            })
            .unwrap();
    }
}

#[cfg(test)]
mod locale_tests {
    use crate::export::SilentRender;
    use ravel_ui::export::ExportError;

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

    /// Every string the export dialog and the render queue panel render
    /// exists in **every** locale, not just the English fallback: the
    /// `ravel-ui` coverage tests only walk `en.toml`, so a missing Japanese
    /// entry would otherwise show English silently.
    #[test]
    fn every_locale_carries_the_export_keys() {
        let mut keys: Vec<&'static str> = vec![
            "menu.file.export",
            "export.title",
            "export.submit",
            "export.browse",
            "export.field.composition",
            "export.field.range",
            "export.field.format",
            "export.field.directory",
            "export.field.prefix",
            "export.field.suffix",
            "export.field.padding",
            "export.field.overwrite",
            "export.field.audio",
            "export.field.audio_no_decoder",
            "export.field.audio_silent_composition",
            "export.error.no_gpu",
            "export.unavailable.no_writer",
            "export.unavailable.ffmpeg_not_linked",
            "export.unavailable.ffmpeg_encoder_missing",
            "export.unavailable.platform_api",
            "export.unavailable.no_platform_route",
            "export.unavailable.not_offered",
            "export.notice.completed_title",
            "export.notice.completed_message",
            "export.notice.failed_title",
            "export.notice.failed_message",
            "export.notice.warning_title",
            "export.notice.audio_failed_message",
            "export.notice.audio_exists_message",
            "export.warning.audio_source_skipped",
            "panel.render_queue",
            "render_queue.empty",
            "render_queue.clear_finished",
            "render_queue.cancel",
            "render_queue.frames",
            "render_queue.state.queued",
            "render_queue.state.running",
            "render_queue.state.completed",
            "render_queue.state.cancelled",
            "render_queue.state.failed",
        ];
        // Driven off the enum rather than listed, so a refusal added later
        // cannot reach the dialog without a sentence behind it.
        keys.extend(
            [
                ExportError::NoComposition,
                ExportError::InvalidStart,
                ExportError::InvalidEnd,
                ExportError::EmptyRange,
                ExportError::RangeOverflow,
                ExportError::NoWriter,
                ExportError::MissingDirectory,
                ExportError::InvalidPadding,
                ExportError::OutputName,
            ]
            .into_iter()
            .map(ExportError::message_key),
        );
        // Likewise for the reasons a render comes out silent: a new one must
        // not be able to reach a notification without a sentence.
        keys.extend(
            [SilentRender::NotAsked, SilentRender::NoDecoder]
                .into_iter()
                .map(SilentRender::message_key),
        );

        for locale in ["en", "ja"] {
            let catalog = catalog(locale);
            for key in &keys {
                assert!(
                    has_key(&catalog, key),
                    "{locale}.toml is missing the export key \"{key}\""
                );
            }
        }
    }
}

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

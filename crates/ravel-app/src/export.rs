// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The session's render queue (`render-export-plan.md`, unit 5).
//!
//! This is the whole of the application's side of exporting: it owns the
//! [`RenderQueue`] the frames go through, the soundtrack that is written
//! beside them, and the rows the render queue panel draws. The dialog
//! collects the request ([`ravel_ui::export`]); this turns it into a job and
//! keeps track of what came back.
//!
//! # Why a second worker
//!
//! The interactive [`EvalService`](ravel_core::runtime::EvalService) drops
//! everything but the newest request, which is right for a parameter scrub
//! and wrong for a render where every frame is a deliverable. So the queue
//! runs its own [`Evaluator`](ravel_core::eval::Evaluator) behind its own
//! [`GpuEvalHooks`] — a **separate hooks instance**, because sharing the
//! interactive one would put export frames and preview frames in one cache,
//! which is the coupling this worker exists to avoid. What the two do share
//! is the `GpuContext` (REQ-GPU-001 puts the pipeline on one device) and the
//! `SharedCacheBudget` (one authority for the memory limit), both taken from
//! [`ProjectState`](crate::project_state::ProjectState).
//!
//! # Crossing back to the UI thread
//!
//! [`RenderQueue`] calls its event callback **on the worker thread**, so the
//! callback does the only thing it may: push the event into an unbounded
//! channel. A detached task on the foreground executor drains it and folds it
//! into this entity — the same shape `ProjectState` uses for evaluation
//! results, and the reason no `Global<Option<Event>>` appears anywhere here
//! (`.agents/rules/gpui.md`).
//!
//! # What a discarded queue does
//!
//! [`RenderQueue`]'s own documentation leaves this to the export UI: a queue
//! that is dropped drains what was already submitted rather than abandoning
//! it, which would keep a whole render running after the window closed. The
//! session therefore **cancels every unfinished job** as it goes away. Each
//! one stops at its next frame boundary and removes its partial output, so
//! closing the window costs one frame rather than one render, and leaves no
//! half-written sequence behind.

use futures::StreamExt as _;
use gpui::{App, Context, Entity, EventEmitter, Global, SharedString, WeakEntity};
use ravel_audio::MixerConfig;
use ravel_core::composition::Document;
use ravel_core::id::CompId;
use ravel_core::runtime::{
    OverwritePolicy, RenderEvent, RenderJob, RenderJobId, RenderQueue, occupied,
};
use ravel_i18n::t;
use ravel_media::encode::{ImageSequenceEncoder, WavWriter};
use ravel_ui::export::ExportRequest;
use ravel_ui::panels::render_queue::RenderQueueRows;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Sample rate of a render's soundtrack.
///
/// Fixed rather than offered as a field, and the same value `ravel-cli` uses:
/// there is no device to ask what the host prefers, and 48 kHz stereo is what
/// every container and editing tool takes without comment. Two front ends
/// that delivered different rates would make one project's exports
/// inconsistent with each other.
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
/// Channels in the delivered mix. See [`AUDIO_SAMPLE_RATE`].
pub const AUDIO_CHANNELS: u32 = 2;

/// Whether this build can decode an audio asset at all.
///
/// Decoding needs FFmpeg, which is an optional feature. A build without it
/// cannot produce a soundtrack, and the honest response is to say so in the
/// dialog rather than to write a silent WAV.
pub const AUDIO_DECODE_AVAILABLE: bool = cfg!(feature = "ffmpeg");

/// Whether `comp` of `document` has any layer carrying audio.
///
/// Scoped to the composition being rendered: the audio option means nothing
/// for a picture-only composition, whatever the rest of the project holds.
pub fn composition_has_audio(document: &Document, comp: CompId) -> bool {
    audio_layer_count(document, comp) > 0
}

/// How many layers of `comp` carry audio.
fn audio_layer_count(document: &Document, comp: CompId) -> usize {
    document.get_composition(comp).map_or(0, |comp| {
        comp.layers
            .iter()
            .filter(|layer| layer.audio.is_some())
            .count()
    })
}

/// Why a render whose composition has sound will not carry any.
///
/// The GUI half of `ravel-cli`'s `Warning::AudioNotRendered`: two different
/// situations for the reader, one they chose and one their build imposed,
/// and both worth saying out loud. Silence that nobody asked for is the
/// failure this exists to prevent — the plan's unit 4 requires a render that
/// drops a composition's sound to say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SilentRender {
    /// The dialog's soundtrack box was cleared.
    NotAsked,
    /// This build has no FFmpeg, so nothing can be decoded.
    NoDecoder,
}

impl SilentRender {
    /// Locale key of the sentence, which carries a `{count}` the caller
    /// fills with the layer count.
    pub fn message_key(self) -> &'static str {
        match self {
            Self::NotAsked => "export.warning.audio_not_rendered",
            Self::NoDecoder => "export.warning.audio_no_decoder",
        }
    }
}

/// Whether this export leaves a composition's sound out, and why.
///
/// `None` when there is nothing to leave out (a picture-only composition) or
/// nothing is left out. The layer count rides along for the sentence, so a
/// project with one stray audio layer reads differently from one built around
/// a mix.
///
/// A function of the document and the request alone — no queue, no device —
/// which is what lets it be asked before anything can fail, and tested
/// without either, exactly as `ravel-cli`'s `plan_audio` is.
pub fn silent_render(
    document: &Document,
    comp: CompId,
    audio_requested: bool,
) -> Option<(SilentRender, usize)> {
    let layers = audio_layer_count(document, comp);
    if layers == 0 {
        return None;
    }
    if !audio_requested {
        return Some((SilentRender::NotAsked, layers));
    }
    (!AUDIO_DECODE_AVAILABLE).then_some((SilentRender::NoDecoder, layers))
}

/// One audio source that was left out of an otherwise complete mix, named the
/// way the user will read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedAudioSource {
    /// The layer's name, or its id when the layer is gone.
    pub layer: String,
    /// The media asset it names.
    pub asset: String,
    /// Why it could not be loaded.
    pub detail: String,
}

/// One export, as the dialog hands it over.
pub struct ExportJob {
    /// The resolved request.
    pub request: ExportRequest,
    /// The document as it was when OK was pressed. The worker renders this
    /// snapshot, so later edits cannot reach the job.
    pub document: Arc<Document>,
    /// Name of the composition, for the queue panel's row.
    pub composition: String,
}

/// Build the worker's job from a resolved export.
///
/// The **one** place the GUI turns a request into work, and deliberately the
/// same three lines `ravel-cli` writes (`crates/ravel-cli/src/execute.rs`):
/// the same [`RenderJob`], the same [`ImageSequenceEncoder`], and one
/// [`ImageSequenceOutput`](ravel_core::media::encode::ImageSequenceOutput)
/// cloned into both the encoder and the job's `output` — the pairing
/// `RenderJob` requires, and what makes a GUI export and a CLI export produce
/// byte-identical sequences.
pub fn build_render_job(request: &ExportRequest, document: Arc<Document>) -> RenderJob {
    RenderJob::new(
        document,
        request.comp,
        request.range.clone(),
        Box::new(ImageSequenceEncoder::new(request.output.clone())),
        request.render_output(),
    )
    .with_overwrite(request.overwrite)
}

/// What the session shows the user about a render.
///
/// An `EventEmitter` rather than a global, because these are one-shot
/// notices: a global would re-fire on unrelated re-renders and coalesce two
/// finished jobs into one (`.agents/rules/gpui.md`).
#[derive(Clone, Debug)]
pub enum RenderServiceEvent {
    /// Every frame of the range was written.
    Completed {
        /// Where the frames are.
        directory: PathBuf,
        frames: u64,
    },
    /// The export did not happen, or did not finish. The message is already
    /// localized.
    Failed { message: SharedString },
    /// Something the user should know that does not stop the render — a
    /// deliverable that will be silent, a source that could not be decoded.
    /// The GUI's answer to `ravel-cli`'s warning stream, and the reason a
    /// dropped soundtrack is never quiet about it. Already localized.
    Warning { message: SharedString },
}

/// The session's render queue.
///
/// Owned by [`RavelWorkspace`](crate::workspace::RavelWorkspace) and reached
/// from panels through [`RenderServiceHandle`], the same shape the audio
/// service uses: the queue outlives every individual panel, so a render keeps
/// going when the render queue panel is closed.
pub struct RenderService {
    /// Spawned on the first submission — a session that never exports never
    /// starts a worker thread or a second `Evaluator`.
    queue: Option<RenderQueue>,
    /// Handed to the queue's event callback, which runs on the worker thread.
    events: futures::channel::mpsc::UnboundedSender<RenderEvent>,
    rows: RenderQueueRows,
    /// Soundtracks written but not yet at the name they are for, by the job
    /// that has to earn them. Removing an entry drops it, which deletes the
    /// file — that is the whole cleanup path for a cancelled or failed
    /// render.
    audio: HashMap<RenderJobId, PendingAudio>,
}

impl RenderService {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (events, mut incoming) = futures::channel::mpsc::unbounded::<RenderEvent>();
        cx.spawn(async move |this, cx| {
            while let Some(event) = incoming.next().await {
                if this
                    .update(cx, |this, cx| this.on_render_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            queue: None,
            events,
            rows: RenderQueueRows::default(),
            audio: HashMap::new(),
        }
    }

    /// The rows the render queue panel draws.
    pub fn rows(&self) -> &RenderQueueRows {
        &self.rows
    }

    /// Queue an export.
    ///
    /// Returns immediately. A soundtrack is mixed and written **before** the
    /// frames are queued (as `ravel-cli` does, so a failure to produce the
    /// sound is reported before a long render rather than after it), on the
    /// background executor — mixing decodes every audio asset of the range
    /// and must not touch the UI thread.
    ///
    /// The mix is one call for the whole range, so a job cannot be cancelled
    /// while it runs — the row does not exist yet. Left that way on purpose:
    /// splitting the mix into cancellable chunks buys nothing until a range
    /// is long enough for the wait to be felt, and the queue's own
    /// cancellation already covers every frame after it.
    pub fn submit(&mut self, job: ExportJob, cx: &mut Context<Self>) {
        // Before anything can refuse the job: a deliverable that will be
        // silent is a fact about the *request*, so it is said even when the
        // submission then fails for want of a device.
        if let Some((reason, layers)) =
            silent_render(&job.document, job.request.comp, job.request.audio)
        {
            cx.emit(RenderServiceEvent::Warning {
                message: SharedString::from(
                    t!(reason.message_key()).replace("{count}", &layers.to_string()),
                ),
            });
        }
        if let Err(message) = self.ensure_queue(cx) {
            cx.emit(RenderServiceEvent::Failed { message });
            return;
        }
        let Some(prepare) = self.audio_task(&job, cx) else {
            self.enqueue(job, None, cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            let prepared = prepare.await;
            let _ = this.update(cx, |this, cx| match prepared {
                Ok(prepared) => {
                    // The mix is the only place a source's failure is seen,
                    // and it is seen on a background thread; the sentences are
                    // built here, where the locale catalog and the user are.
                    for skipped in &prepared.skipped {
                        cx.emit(RenderServiceEvent::Warning {
                            message: SharedString::from(
                                t!("export.warning.audio_source_skipped")
                                    .replace("{layer}", &skipped.layer)
                                    .replace("{asset}", &skipped.asset)
                                    .replace("{detail}", &skipped.detail),
                            ),
                        });
                    }
                    this.enqueue(job, prepared.pending, cx)
                }
                Err(message) => cx.emit(RenderServiceEvent::Failed {
                    message: SharedString::from(message),
                }),
            });
        })
        .detach();
    }

    /// Ask a job to stop. A queued one never starts; a running one stops at
    /// its next frame boundary and removes its partial output.
    pub fn cancel(&mut self, job: RenderJobId) {
        if let Some(queue) = &self.queue {
            queue.cancel(job);
        }
    }

    /// Drop the rows of jobs that have stopped.
    pub fn clear_finished(&mut self, cx: &mut Context<Self>) {
        if self.rows.clear_finished() {
            cx.notify();
        }
    }

    /// Open a row and fold events into it without a worker behind them.
    ///
    /// Test-only, and the whole reason it exists is that the render queue
    /// panel's drawing is the one part of this that needs a *populated*
    /// service: submitting for real wants a GPU adapter, which the machines
    /// running the tests may not have.
    #[cfg(test)]
    pub(crate) fn record_for_test(
        &mut self,
        job: RenderJobId,
        composition: &str,
        directory: &str,
        total_frames: u64,
        events: &[RenderEvent],
    ) {
        self.rows
            .submitted(job, composition, directory, total_frames);
        for event in events {
            self.rows.observe(event);
        }
    }

    /// Spawn the worker if this session has not needed one yet.
    ///
    /// Fails only when there is no GPU: the render evaluator is the same
    /// pipeline the viewer runs, so a session that could not build a device
    /// cannot render either — and says so here rather than producing a job
    /// that fails on its first frame.
    fn ensure_queue(&mut self, cx: &mut Context<Self>) -> Result<(), SharedString> {
        if self.queue.is_some() {
            return Ok(());
        }
        let project = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade())
            .ok_or_else(|| SharedString::from(t!("export.error.no_gpu")))?;
        let (gpu, budget) = {
            let project = project.read(cx);
            let gpu = project.gpu_context().cloned();
            let budget = project.cache_budget().cloned();
            match (gpu, budget) {
                (Some(gpu), Some(budget)) => (gpu, budget),
                _ => return Err(SharedString::from(t!("export.error.no_gpu"))),
            }
        };
        let events = self.events.clone();
        self.queue = Some(RenderQueue::spawn_with_budget(
            // A second hooks instance on the shared device: the caches stay
            // apart, the wgpu device does not.
            ravel_nodes::GpuEvalHooks::with_budget(gpu, budget.clone()),
            budget,
            move |event| {
                // On the worker thread. Everything else happens in
                // `on_render_event`, on the UI thread.
                let _ = events.unbounded_send(event);
            },
        ));
        Ok(())
    }

    /// The background work that produces a soundtrack, or `None` when this
    /// render has none.
    fn audio_task(
        &self,
        job: &ExportJob,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Task<Result<PreparedAudio, String>>> {
        if !job.request.audio || !AUDIO_DECODE_AVAILABLE {
            return None;
        }
        if !composition_has_audio(&job.document, job.request.comp) {
            return None;
        }
        let document = job.document.clone();
        let comp = job.request.comp;
        let range = job.request.range.clone();
        let destination = job.request.audio_path();
        let overwrite = job.request.overwrite;
        Some(
            cx.background_executor().spawn(async move {
                write_soundtrack(&document, comp, range, destination, overwrite)
            }),
        )
    }

    /// Hand the frames to the worker and open the panel's row.
    fn enqueue(&mut self, job: ExportJob, audio: Option<PendingAudio>, cx: &mut Context<Self>) {
        let Some(queue) = self.queue.as_mut() else {
            // `submit` established the queue before anything could await, so
            // this only happens if the entity was rebuilt underneath the
            // task — report rather than drop the request silently.
            cx.emit(RenderServiceEvent::Failed {
                message: SharedString::from(t!("export.error.no_gpu")),
            });
            return;
        };
        let render_job = build_render_job(&job.request, job.document);
        let id = queue.submit(render_job);
        self.rows.submitted(
            id,
            job.composition,
            job.request.output.directory(),
            job.request.frame_count(),
        );
        if let Some(audio) = audio {
            self.audio.insert(id, audio);
        }
        cx.notify();
    }

    /// Fold one worker event in. Runs on the UI thread.
    fn on_render_event(&mut self, event: RenderEvent, cx: &mut Context<Self>) {
        match &event {
            RenderEvent::Completed { job, frames } => {
                let frames = *frames;
                let directory = self
                    .rows
                    .rows()
                    .iter()
                    .find(|row| row.job() == *job)
                    .map(|row| row.directory().to_path_buf())
                    .unwrap_or_default();
                self.publish_audio(*job, directory, frames, cx);
            }
            RenderEvent::Cancelled { job, .. } => {
                // Dropping the pending mix removes the file it wrote: a
                // deliverable that never completed must not leave a
                // soundtrack behind for frames that are not there.
                self.audio.remove(job);
            }
            RenderEvent::Failed { job, error } => {
                self.audio.remove(job);
                cx.emit(RenderServiceEvent::Failed {
                    message: SharedString::from(format!(
                        "{}\n{error}",
                        t!("export.notice.failed_message")
                    )),
                });
            }
            RenderEvent::Started { .. } | RenderEvent::Progress { .. } => {}
        }
        if self.rows.observe(&event) {
            cx.notify();
        }
    }

    /// Put a finished render's soundtrack at its real name, then announce the
    /// export.
    ///
    /// The rename runs on the background executor with the rest of the
    /// filesystem work, and the notice waits for it: a message saying the
    /// export is done while the WAV is still under its temporary name would
    /// describe a deliverable that does not exist yet.
    fn publish_audio(
        &mut self,
        job: RenderJobId,
        directory: PathBuf,
        frames: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.audio.remove(&job) else {
            cx.emit(RenderServiceEvent::Completed { directory, frames });
            return;
        };
        let publish = cx
            .background_executor()
            .spawn(async move { pending.publish() });
        cx.spawn(async move |this, cx| {
            let outcome = publish.await;
            let _ = this.update(cx, |_this, cx| match outcome {
                Ok(_) => cx.emit(RenderServiceEvent::Completed { directory, frames }),
                Err(error) => cx.emit(RenderServiceEvent::Failed {
                    message: SharedString::from(format!(
                        "{}\n{error}",
                        t!("export.notice.audio_failed_message")
                    )),
                }),
            });
        })
        .detach();
    }
}

impl EventEmitter<RenderServiceEvent> for RenderService {}

/// Cancels what is still outstanding; see the module note on a discarded
/// queue. Dropping the [`RenderQueue`] afterwards closes its channel without
/// joining, so the UI thread is never blocked for the length of a render.
impl Drop for RenderService {
    fn drop(&mut self) {
        let Some(queue) = &self.queue else {
            return;
        };
        for job in self.rows.unfinished() {
            queue.cancel(job);
        }
    }
}

/// Durable registry of the session's render queue. Panels reach it through
/// this rather than owning one, because the queue outlives them.
pub struct RenderServiceHandle(pub WeakEntity<RenderService>);

impl Global for RenderServiceHandle {}

/// The live render service, if this session has one.
pub fn render_service(cx: &App) -> Option<Entity<RenderService>> {
    cx.try_global::<RenderServiceHandle>()?.0.upgrade()
}

// ---------------------------------------------------------------------------
// Soundtrack
// ---------------------------------------------------------------------------

/// Mix `range` of `comp` and write it beside where it belongs.
///
/// Runs on the background executor: it decodes every audio asset the range
/// touches. Returns `None` when the composition turns out to carry no audio
/// after all — the caller already asked, but the document is the authority.
///
/// A source that cannot be decoded is a **warning**, not a failure: the
/// picture is still worth having and the mix is still the right length, so
/// the deliverable stays in sync. This mirrors `ravel-cli`'s `render_audio`,
/// which is the reference for the whole soundtrack path — including that the
/// notes reach the user: they are carried back in
/// [`PreparedAudio::skipped`] and become notifications, because a source that
/// went missing only in the log is a deliverable that is quietly wrong.
fn write_soundtrack(
    document: &Document,
    comp: CompId,
    range: std::ops::Range<u64>,
    destination: PathBuf,
    overwrite: OverwritePolicy,
) -> Result<PreparedAudio, String> {
    // The frames' conflict check knows nothing about the WAV beside them
    // (`RenderOutput::conflicts` describes frames), so a render that must not
    // overwrite has to ask about this name itself.
    if overwrite == OverwritePolicy::Refuse && occupied(&destination) {
        return Err(format!(
            "{}\n{}",
            t!("export.notice.audio_exists_message"),
            destination.display()
        ));
    }

    let config = MixerConfig {
        output_sample_rate: AUDIO_SAMPLE_RATE,
        output_channels: AUDIO_CHANNELS,
    };
    let Some(mix) = ravel_audio::offline::mix_range(document, comp, range, &config) else {
        return Ok(PreparedAudio::default());
    };
    let mut skipped_sources = Vec::with_capacity(mix.skipped.len());
    for skipped in &mix.skipped {
        tracing::warn!(
            layer = skipped.layer_id.raw(),
            asset = %skipped.asset_id,
            reason = %skipped.reason,
            "an audio source was left out of the render"
        );
        skipped_sources.push(SkippedAudioSource {
            // The name the user knows the layer by; the id is the fallback
            // for a layer the document no longer has.
            layer: document
                .get_composition(comp)
                .and_then(|comp| comp.layers.iter().find(|l| l.id == skipped.layer_id))
                .map(|layer| layer.name.clone())
                .unwrap_or_else(|| skipped.layer_id.raw().to_string()),
            asset: skipped.asset_id.clone(),
            detail: skipped.reason.clone(),
        });
    }

    // The image encoder makes this directory in `begin`, and the sound is
    // written first, so it may not exist yet.
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }

    // Everything below writes the *temporary* name; the real one is not
    // opened, truncated or created before the rename, which leaves an
    // existing soundtrack intact until there is a new render to replace it.
    // A `WavWriter` that fails or is dropped unfinished removes its own file.
    let temporary = temporary_path(&destination);
    let mut writer = WavWriter::create(&temporary, mix.buffer.sample_rate, mix.buffer.channels)
        .map_err(|error| error.to_string())?;
    writer
        .write_samples(&mix.buffer.data)
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())?;

    Ok(PreparedAudio {
        pending: Some(PendingAudio {
            destination,
            temporary,
        }),
        skipped: skipped_sources,
    })
}

/// What a finished mix leaves for the UI thread: the file to publish, and
/// everything the user has to be told about it.
#[derive(Default)]
struct PreparedAudio {
    /// The written mix, or `None` when the composition turned out to carry no
    /// audio after all.
    pending: Option<PendingAudio>,
    /// Sources that could not be decoded. Structured rather than localized
    /// here: this is built on the background executor, and the sentences
    /// belong where the user is.
    skipped: Vec<SkippedAudioSource>,
}

/// A finished mix, written beside where it belongs and waiting for the render
/// to earn it.
///
/// **Dropping this removes the file.** That is the cleanup path for a render
/// that fails, is cancelled, or never starts: the sound is written first, so
/// every one of those exits passes through this drop and none of them reaches
/// the name the deliverable is under. [`publish`](Self::publish) is the only
/// thing that does.
struct PendingAudio {
    destination: PathBuf,
    /// The file actually written. Emptied by [`publish`](Self::publish), so
    /// [`Drop`] knows whether there is anything left to take back.
    temporary: PathBuf,
}

impl PendingAudio {
    /// Put the mix at its real name, and return that name.
    ///
    /// Atomic, because the temporary file shares the destination's directory.
    fn publish(mut self) -> Result<PathBuf, String> {
        std::fs::rename(&self.temporary, &self.destination).map_err(|error| {
            format!(
                "cannot put the soundtrack at {}: {error}",
                self.destination.display()
            )
        })?;
        self.temporary = PathBuf::new();
        Ok(self.destination.clone())
    }
}

impl Drop for PendingAudio {
    fn drop(&mut self) {
        if self.temporary.as_os_str().is_empty() {
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.temporary)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.temporary.display(),
                %error,
                "could not remove the unfinished render's audio"
            );
        }
    }
}

/// The name a mix is written under until the render has earned the real one.
///
/// **Beside the destination, never in a system temporary directory**:
/// publication is a `rename`, which is atomic within one filesystem and a
/// copy-then-delete across two — and a copy that fails halfway is exactly the
/// half-written deliverable this design rules out. The leading dot keeps it
/// out of a listing; the process id and the serial keep two renders into one
/// directory off each other's file.
fn temporary_path(destination: &Path) -> PathBuf {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let name = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    destination.with_file_name(format!(".{name}.{}-{serial}.part", std::process::id()))
}

// Named imports rather than a `use super::*;` glob: this file expands the
// gpui proc macros, and the glob crashes rustc 1.95 there (the same
// constraint `export_dialog.rs` and `panels/mod.rs` record).
#[cfg(test)]
mod tests {
    use super::{
        AUDIO_DECODE_AVAILABLE, ExportJob, RenderService, RenderServiceEvent, SilentRender,
        silent_render,
    };
    use gpui::AppContext as _;
    use ravel_core::composition::{AudioSource, Composition, Document, Layer};
    use ravel_core::graph::Graph;
    use ravel_core::id::{CompId, LayerId};
    use ravel_core::types::FrameRate;
    use ravel_ui::export::{ExportRequest, ExportSettings};
    use std::sync::Arc;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };
    const DURATION: u64 = 12;

    fn comp_id() -> CompId {
        CompId::new(1)
    }

    /// A one-layer composition, with or without a soundtrack on that layer.
    fn document(with_audio: bool) -> Arc<Document> {
        let mut layer = Layer::new(LayerId::new(1), "voice over", Graph::new());
        if with_audio {
            layer.audio = Some(AudioSource::new("voice", 0));
        }
        let mut comp = Composition::new(comp_id(), "shot 010", (32, 18), FPS, DURATION);
        comp.layers.push_back(layer);
        Arc::new(Document::new(Graph::new()).with_composition(comp))
    }

    fn request(directory: &std::path::Path, audio: bool) -> ExportRequest {
        let mut settings =
            ExportSettings::for_composition(comp_id(), "shot 010", DURATION, directory.to_owned());
        settings.audio = audio;
        settings.resolve().expect("the default form resolves")
    }

    /// The GUI's half of `ravel-cli`'s
    /// `a_project_with_audio_warns_that_the_render_is_silent`: a composition
    /// with sound rendered without it is a warning, and the reason says which
    /// of the two situations it is.
    #[test]
    fn a_composition_with_sound_rendered_without_it_is_a_warning() {
        assert_eq!(
            silent_render(&document(true), comp_id(), false),
            Some((SilentRender::NotAsked, 1)),
            "clearing the soundtrack box is still worth saying out loud",
        );

        // Asked for: whether it happens is the build's answer, not the
        // project's, and the two reasons must not be confused.
        let asked = silent_render(&document(true), comp_id(), true);
        if AUDIO_DECODE_AVAILABLE {
            assert_eq!(asked, None, "the soundtrack is being written");
        } else {
            assert_eq!(asked, Some((SilentRender::NoDecoder, 1)));
        }

        // A picture-only composition has nothing to lose and says nothing —
        // the CLI's `a_project_without_audio_says_nothing`.
        assert_eq!(silent_render(&document(false), comp_id(), false), None);
        assert_eq!(silent_render(&document(false), comp_id(), true), None);
    }

    #[derive(Default)]
    struct EventRecorder(Vec<RenderServiceEvent>);

    fn record(
        service: &gpui::Entity<RenderService>,
        cx: &mut gpui::TestAppContext,
    ) -> gpui::Entity<EventRecorder> {
        let recorder = cx.new(|_| EventRecorder::default());
        recorder.update(cx, |_, cx| {
            cx.subscribe(
                service,
                |recorder, _service, event: &RenderServiceEvent, _cx| {
                    recorder.0.push(event.clone());
                },
            )
            .detach();
        });
        recorder
    }

    fn warnings(
        recorder: &gpui::Entity<EventRecorder>,
        cx: &mut gpui::TestAppContext,
    ) -> Vec<String> {
        recorder.read_with(cx, |recorder, _| {
            recorder
                .0
                .iter()
                .filter_map(|event| match event {
                    RenderServiceEvent::Warning { message } => Some(message.to_string()),
                    _ => None,
                })
                .collect()
        })
    }

    /// The warning is on the submission path itself, so it reaches the user
    /// whatever happens to the job afterwards — this session has no GPU, and
    /// the export fails, and the notice about the missing sound still arrives.
    #[gpui::test]
    fn submitting_an_export_that_drops_the_sound_warns(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().expect("temp dir");
        let service = cx.new(RenderService::new);
        let recorder = record(&service, cx);

        service.update(cx, |service, cx| {
            service.submit(
                ExportJob {
                    request: request(dir.path(), false),
                    document: document(true),
                    composition: "shot 010".to_owned(),
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(
            warnings(&recorder, cx).len(),
            1,
            "a composition with sound exported without it must say so exactly once",
        );
    }

    /// And nothing is said when there is nothing to say: a picture-only
    /// composition must not train the user to dismiss the notice.
    #[gpui::test]
    fn submitting_a_picture_only_export_says_nothing(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().expect("temp dir");
        let service = cx.new(RenderService::new);
        let recorder = record(&service, cx);

        service.update(cx, |service, cx| {
            service.submit(
                ExportJob {
                    request: request(dir.path(), false),
                    document: document(false),
                    composition: "shot 010".to_owned(),
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert!(warnings(&recorder, cx).is_empty());
    }
}

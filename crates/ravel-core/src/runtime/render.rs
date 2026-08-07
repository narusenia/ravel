// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sequential render worker and job queue
//! (`docs/implementation/render-export-plan.md`, unit 2).
//!
//! # Why not [`EvalService`](super::EvalService)
//!
//! The interactive service is **latest-wins**: it drains everything queued
//! behind the request it picked up and evaluates only the newest one, which is
//! exactly right for a parameter scrub and exactly wrong for a render, where
//! every frame of the range is a deliverable. So this is a second worker
//! rather than a mode of the first.
//!
//! It also runs its own [`Evaluator`]. On the GPU side that costs nothing —
//! the host's hooks hold a cloned `GpuContext`, so the wgpu device and queue
//! are shared — and what it does duplicate, the CPU-side result cache and the
//! processor registrations, is duplication worth having: rendering frame 500
//! must not evict the frame the user is looking at, and a render must never
//! be handed a cached preview-grade value.
//!
//! # Snapshot semantics
//!
//! A job carries `Arc<Document>`. The document a job renders is therefore the
//! one that was submitted, whatever the user does to their copy afterwards —
//! `Document` is immutable-by-clone, so no extra machinery is needed.
//!
//! # Refusing to overwrite
//!
//! [`Encoder::abort`] deliberately keeps files it did not create, so
//! re-rendering over an existing sequence and then cancelling leaves a
//! sequence that is new up to the frame reached and old after it — mixed
//! content under names that look complete. The worker therefore checks the
//! output **before evaluating anything** and fails the job unless the
//! submitter opted in with [`OverwritePolicy::Replace`].
//!
//! The check is per file name, never "is the directory empty": the plan
//! guarantees that N processes can split one range across the same output
//! directory (`--range`), and those processes write disjoint file names.
//!
//! It is a pre-flight guard, not a lock. The encoder places each frame with a
//! rename, and a rename replaces — so a writer that creates one of these names
//! *after* the check still wins. That window is `MED-MED-06`: closing it means
//! a no-replace rename per platform, which belongs to the encoder rather than
//! here and wants a Windows CI run behind it.

use crate::cache_budget::SharedCacheBudget;
use crate::composition::Document;
use crate::composition::compile::{CompileError, compile_composition};
use crate::eval::{EvalContext, EvalError, Evaluator, Precision, Quality};
use crate::graph::Graph;
use crate::id::CompId;
use crate::media::MediaError;
use crate::media::encode::{Encoder, ImageSequenceOutput};
use crate::runtime::eval_service::{EvalWorkerHooks, InvalidationHint, ProcessorSync};
use crate::types::FrameBuffer;
use crossbeam_channel::{Sender, unbounded};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use thiserror::Error;

/// How many conflicting paths [`RenderError::OutputExists`] carries.
///
/// The count is exact; the list is a sample, because a re-render of a
/// 10 000-frame sequence conflicts with all of it and nobody reads that.
pub const CONFLICT_SAMPLE: usize = 8;

// ===========================================================================
// Job description
// ===========================================================================

/// Identifies a submitted job in the queue's events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RenderJobId(u64);

impl RenderJobId {
    /// The underlying number, for logging and machine-readable output.
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for RenderJobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a job's output occupies on disk.
///
/// Only used to decide whether a render would land on existing files, which
/// is why it describes *names* rather than carrying an encoder: the encoder
/// knows how to write, this knows what would be written over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderOutput {
    /// A numbered still-image sequence: one file per frame, named from the
    /// absolute frame number.
    Sequence(ImageSequenceOutput),
    /// A container holding the whole range in a single file.
    Container(PathBuf),
}

impl RenderOutput {
    /// Every path this output occupies when `range` is rendered.
    ///
    /// One entry per frame for a sequence — which is what makes the conflict
    /// check file-name granular, so two processes splitting one range into
    /// the same directory never collide — and one entry for a container,
    /// which holds the range whole.
    pub fn occupied_paths(&self, range: Range<u64>) -> Vec<PathBuf> {
        match self {
            Self::Sequence(sequence) => range.map(|frame| sequence.frame_path(frame)).collect(),
            Self::Container(path) => vec![path.clone()],
        }
    }

    /// The subset of [`Self::occupied_paths`] that something already
    /// occupies — the whole of what [`OverwritePolicy::Refuse`] means.
    ///
    /// Public because a front end wants to refuse a doomed render *earlier*
    /// than the worker can: `ravel-cli` calls this before it builds a GPU
    /// context, so a machine with no adapter still reports an existing
    /// output as one. The worker's own check remains the authoritative one —
    /// it runs at the instant the job starts, which is the only moment the
    /// answer is not already stale — and calls this same function, so
    /// "already there" has one definition rather than two.
    pub fn conflicts(&self, range: Range<u64>) -> Vec<PathBuf> {
        self.occupied_paths(range)
            .into_iter()
            .filter(|path| occupied(path))
            .collect()
    }
}

/// Whether a job may write where files already are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OverwritePolicy {
    /// Fail the job before evaluating a single frame if any output file
    /// already exists. The default, because the alternative silently produces
    /// a sequence that is part new and part old.
    #[default]
    Refuse,
    /// Write regardless. The submitter has decided the existing output is
    /// theirs to replace.
    Replace,
}

/// One unit of render work.
///
/// `output` must describe what `encoder` writes: the worker asks `output`
/// which files the job would occupy and hands the frames to `encoder`, so a
/// mismatched pair checks one destination and writes another. Build both from
/// the same description — for a sequence, that is one [`ImageSequenceOutput`]
/// value cloned into both.
pub struct RenderJob {
    /// The document as it was when the job was submitted. Later edits to the
    /// submitter's copy cannot reach it.
    pub document: Arc<Document>,
    /// Which composition of `document` to render.
    pub comp: CompId,
    /// Half-open range of absolute frame numbers.
    pub range: Range<u64>,
    /// Receives the frames. Driven `begin` → `write_frame`\* → `finish`, or
    /// `abort` when the job is cancelled or fails.
    pub encoder: Box<dyn Encoder>,
    /// What the job occupies on disk (see the type's note on pairing).
    pub output: RenderOutput,
    /// Whether existing output is an error.
    pub overwrite: OverwritePolicy,
}

impl RenderJob {
    /// A job that refuses to overwrite existing output.
    pub fn new(
        document: Arc<Document>,
        comp: CompId,
        range: Range<u64>,
        encoder: Box<dyn Encoder>,
        output: RenderOutput,
    ) -> Self {
        Self {
            document,
            comp,
            range,
            encoder,
            output,
            overwrite: OverwritePolicy::Refuse,
        }
    }

    /// Allow the job to write over files that are already there.
    pub fn with_overwrite(mut self, overwrite: OverwritePolicy) -> Self {
        self.overwrite = overwrite;
        self
    }

    /// Number of frames the job would produce.
    pub fn frame_count(&self) -> u64 {
        self.range.end.saturating_sub(self.range.start)
    }
}

// ===========================================================================
// Errors and events
// ===========================================================================

/// Why a render job did not complete.
#[derive(Debug, Error)]
pub enum RenderError {
    /// The job named a composition the submitted document does not hold.
    #[error("composition {0} is not in the submitted document")]
    CompositionNotFound(CompId),

    /// The range covers no frames, which is always a mistake at the caller
    /// rather than a job that trivially succeeds.
    #[error("frame range {start}..{end} covers no frames")]
    EmptyRange { start: u64, end: u64 },

    /// The composition could not be compiled into a graph.
    #[error("compiling the composition failed: {0}")]
    Compile(#[from] CompileError),

    /// Output files already exist and the job did not opt into replacing
    /// them. Reported before any frame is evaluated.
    #[error(
        "{total} output file(s) already exist (e.g. {first}); rendering would mix new frames \
         into the existing output",
        first = .sample.first().map_or_else(|| "?".to_string(), |p| p.display().to_string()),
    )]
    OutputExists {
        /// Up to [`CONFLICT_SAMPLE`] of the conflicting paths.
        sample: Vec<PathBuf>,
        /// How many conflicting paths there are in total.
        total: usize,
    },

    /// Evaluating a frame failed. The job stops; the queue continues.
    #[error("evaluating frame {frame} failed: {source}")]
    Eval {
        frame: u64,
        #[source]
        source: EvalError,
    },

    /// The composition output evaluated to something that is not a picture.
    #[error("frame {frame} evaluated to a value that is not a frame buffer")]
    NotAFrame { frame: u64 },

    /// The encoder refused a frame, or could not open or close its output.
    #[error("encoding failed: {0}")]
    Encode(#[from] MediaError),

    /// The job never reached the worker, because the worker thread is gone —
    /// it panicked out of a hook or an event callback. Reported by
    /// [`RenderQueue::submit`] rather than by the worker, for the obvious
    /// reason.
    #[error("the render worker thread is gone; the job was not queued")]
    WorkerGone,
}

/// What the queue reports as it works.
///
/// Every job that reaches the worker emits [`RenderEvent::Started`] and then
/// exactly one terminal event ([`is_terminal`](RenderEvent::is_terminal)).
/// Callbacks run on the worker thread: forward them to the UI through a
/// channel rather than doing work in them.
#[derive(Debug)]
pub enum RenderEvent {
    /// The job has been picked up.
    Started {
        job: RenderJobId,
        /// Frames the range covers.
        total_frames: u64,
    },
    /// A frame has been written.
    Progress {
        job: RenderJobId,
        /// Absolute number of the frame just written.
        frame: u64,
        /// Frames written so far.
        rendered: u64,
        total_frames: u64,
    },
    /// Every frame of the range was written and the output was closed.
    Completed { job: RenderJobId, frames: u64 },
    /// The job stopped at a frame boundary and its partial output was
    /// removed.
    Cancelled {
        job: RenderJobId,
        /// Frames written before the cancellation took effect. They no longer
        /// exist; the count says how far the job got.
        frames_rendered: u64,
    },
    /// The job failed. Its partial output was removed; the queue moved on to
    /// the next job.
    Failed {
        job: RenderJobId,
        error: RenderError,
    },
}

impl RenderEvent {
    /// Which job this is about.
    pub fn job(&self) -> RenderJobId {
        match self {
            Self::Started { job, .. }
            | Self::Progress { job, .. }
            | Self::Completed { job, .. }
            | Self::Cancelled { job, .. }
            | Self::Failed { job, .. } => *job,
        }
    }

    /// Whether this is the job's last event.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled { .. } | Self::Failed { .. }
        )
    }
}

// ===========================================================================
// Presenting the events
// ===========================================================================

/// Where a job has got to, as the last event about it left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    /// Picked up; frames are being written.
    Running,
    /// Every frame of the range was written.
    Completed,
    /// Stopped at a frame boundary; the partial output is gone.
    Cancelled,
    /// Failed. The message is [`RenderError`]'s own `Display`, because a
    /// presenter outlives the borrowed event and the error is not `Clone`.
    /// It is a diagnostic, not a user-facing sentence: a caller that wants a
    /// localized one classifies from the [`RenderError`] before it folds the
    /// event in.
    Failed { message: String },
}

/// One job's progress, folded from the [`RenderEvent`]s about it.
///
/// **Deliberately not in the CLI.** Turning a stream of events into "job 3,
/// 47 of 120 frames, running" is the same arithmetic for `ravel-cli`'s
/// progress line and for the render queue panel (`EXPORT-5`), and a copy in
/// each is a copy that drifts. What stays with the caller is the presentation
/// — text through `t!`, a bar, a table row — because `ravel-core` holds no
/// user-visible strings.
///
/// Events for another job are ignored rather than merged, so a consumer can
/// hand every event to every tracker it owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobProgress {
    job: RenderJobId,
    total_frames: u64,
    rendered: u64,
    last_frame: Option<u64>,
    state: JobState,
}

impl JobProgress {
    /// Start tracking from a [`RenderEvent::Started`], which is the only
    /// event that carries the frame total. Returns `None` for any other
    /// event, so a consumer can drive creation straight off the stream.
    pub fn started(event: &RenderEvent) -> Option<Self> {
        match event {
            RenderEvent::Started { job, total_frames } => Some(Self {
                job: *job,
                total_frames: *total_frames,
                rendered: 0,
                last_frame: None,
                state: JobState::Running,
            }),
            _ => None,
        }
    }

    /// Fold `event` in. Returns whether it was about this job.
    pub fn observe(&mut self, event: &RenderEvent) -> bool {
        if event.job() != self.job {
            return false;
        }
        match event {
            RenderEvent::Started { total_frames, .. } => {
                self.total_frames = *total_frames;
                self.rendered = 0;
                self.last_frame = None;
                self.state = JobState::Running;
            }
            RenderEvent::Progress {
                frame,
                rendered,
                total_frames,
                ..
            } => {
                self.total_frames = *total_frames;
                self.rendered = *rendered;
                self.last_frame = Some(*frame);
            }
            RenderEvent::Completed { frames, .. } => {
                self.rendered = *frames;
                self.state = JobState::Completed;
            }
            RenderEvent::Cancelled {
                frames_rendered, ..
            } => {
                self.rendered = *frames_rendered;
                self.state = JobState::Cancelled;
            }
            RenderEvent::Failed { error, .. } => {
                self.state = JobState::Failed {
                    message: error.to_string(),
                };
            }
        }
        true
    }

    pub fn job(&self) -> RenderJobId {
        self.job
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// Frames written so far. After a cancellation this is how far the job
    /// got, not how many files remain — there are none.
    pub fn rendered(&self) -> u64 {
        self.rendered
    }

    /// Absolute number of the most recently written frame.
    pub fn last_frame(&self) -> Option<u64> {
        self.last_frame
    }

    pub fn state(&self) -> &JobState {
        &self.state
    }

    /// Whether the job has stopped, however it stopped.
    pub fn is_finished(&self) -> bool {
        !matches!(self.state, JobState::Running)
    }

    /// Fraction of the range written, in `0.0..=1.0`.
    ///
    /// A job with no frames reads as complete rather than as a division by
    /// zero; the worker refuses such a range anyway
    /// ([`RenderError::EmptyRange`]), so this only shields a presenter that
    /// built a tracker before the refusal arrived.
    pub fn fraction(&self) -> f32 {
        if self.total_frames == 0 {
            return 1.0;
        }
        (self.rendered as f32 / self.total_frames as f32).clamp(0.0, 1.0)
    }
}

// ===========================================================================
// Queue
// ===========================================================================

/// Which jobs exist and which of them have been asked to stop.
///
/// Two sets rather than one, because a cancellation can arrive at any moment
/// — including just after the worker finished the job. Keeping the live set
/// means such a request is *dropped* instead of being recorded for an id that
/// will never be looked at again, which is what keeps this bounded by the
/// number of outstanding jobs rather than by the number ever submitted.
#[derive(Default)]
struct CancelState {
    /// Submitted and not yet terminated.
    live: HashSet<RenderJobId>,
    /// Cancellation requests, always a subset of `live`.
    requested: HashSet<RenderJobId>,
}

impl CancelState {
    /// Record a submitted job. Called before it is queued, so a cancellation
    /// racing the submission still finds it.
    fn register(&mut self, job: RenderJobId) {
        self.live.insert(job);
    }

    /// Record a request to stop `job`, ignoring one for a job that has
    /// already terminated.
    fn request(&mut self, job: RenderJobId) {
        if self.live.contains(&job) {
            self.requested.insert(job);
        }
    }

    fn is_requested(&self, job: RenderJobId) -> bool {
        self.requested.contains(&job)
    }

    /// Forget `job` entirely. Both sets, because a request that arrived while
    /// the job was ending is still in `requested`.
    fn retire(&mut self, job: RenderJobId) {
        self.live.remove(&job);
        self.requested.remove(&job);
    }

    /// Outstanding jobs and outstanding requests, for the test that pins the
    /// bound above.
    #[cfg(test)]
    fn sizes(&self) -> (usize, usize) {
        (self.live.len(), self.requested.len())
    }
}

type SharedCancelState = Arc<Mutex<CancelState>>;

/// Handle on the render worker thread.
///
/// Jobs run one at a time in submission order (in-process parallel rendering
/// is out of scope; splitting a range across processes is the supported way
/// to use more machines). Submitting never blocks.
pub struct RenderQueue {
    tx: Option<Sender<(RenderJobId, RenderJob)>>,
    next_id: u64,
    cancel_state: SharedCancelState,
    /// Shared with the worker so [`RenderQueue::submit`] can report a job the
    /// worker will never see. Every other event comes from the worker thread.
    on_event: Arc<dyn Fn(RenderEvent) + Send + Sync>,
    worker: Option<JoinHandle<()>>,
}

impl RenderQueue {
    /// Spawn the worker with an unbounded result cache.
    ///
    /// `hooks` supply processor registration and output post-processing
    /// exactly as they do for [`EvalService`](super::EvalService), and must be
    /// a **second instance**: sharing one with the interactive service would
    /// reintroduce the cache coupling this worker exists to avoid. The GPU
    /// context inside them is cheap to clone and is meant to be shared.
    ///
    /// `on_event` is normally called on the worker thread; the one exception
    /// is a job that could not be queued at all, which [`submit`] reports on
    /// the caller's thread.
    ///
    /// [`submit`]: RenderQueue::submit
    pub fn spawn<H, F>(hooks: H, on_event: F) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(RenderEvent) + Send + Sync + 'static,
    {
        Self::spawn_inner(hooks, None, on_event)
    }

    /// Spawn the worker with a result cache bounded by `budget`.
    pub fn spawn_with_budget<H, F>(hooks: H, budget: SharedCacheBudget, on_event: F) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(RenderEvent) + Send + Sync + 'static,
    {
        Self::spawn_inner(hooks, Some(budget), on_event)
    }

    fn spawn_inner<H, F>(mut hooks: H, budget: Option<SharedCacheBudget>, on_event: F) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(RenderEvent) + Send + Sync + 'static,
    {
        let (tx, rx) = unbounded::<(RenderJobId, RenderJob)>();
        let cancel_state: SharedCancelState = Arc::new(Mutex::new(CancelState::default()));
        let worker_cancel_state = cancel_state.clone();
        let on_event: Arc<dyn Fn(RenderEvent) + Send + Sync> = Arc::new(on_event);
        let worker_on_event = on_event.clone();
        let worker = std::thread::Builder::new()
            .name("ravel-render-worker".into())
            .spawn(move || {
                let mut evaluator = match budget {
                    Some(budget) => Evaluator::with_budget(budget),
                    None => Evaluator::new(),
                };
                while let Ok((id, job)) = rx.recv() {
                    let total_frames = job.frame_count();
                    tracing::info!(
                        job = id.raw(),
                        comp = job.comp.raw(),
                        start = job.range.start,
                        end = job.range.end,
                        "render job picked up"
                    );
                    worker_on_event(RenderEvent::Started {
                        job: id,
                        total_frames,
                    });
                    let event = run_job(
                        &mut evaluator,
                        &mut hooks,
                        id,
                        job,
                        &worker_cancel_state,
                        &*worker_on_event,
                    );
                    // Retiring under the same lock `cancel` takes is what
                    // bounds the state: a request that arrives after this
                    // point finds the job gone and is discarded, and one that
                    // arrived just before it is removed here.
                    worker_cancel_state.lock().expect("cancel state").retire(id);
                    match &event {
                        RenderEvent::Failed { error, .. } => {
                            tracing::warn!(job = id.raw(), %error, "render job failed");
                        }
                        _ => tracing::info!(job = id.raw(), "render job finished"),
                    }
                    worker_on_event(event);
                }
            })
            .expect("failed to spawn render worker");
        Self {
            tx: Some(tx),
            next_id: 0,
            cancel_state,
            on_event,
            worker: Some(worker),
        }
    }

    /// Queue `job` and return its id. Returns immediately.
    ///
    /// Handing the job over can only fail if the worker thread is gone — it
    /// panicked, taking the receiving end with it. The job is then reported
    /// as [`RenderError::WorkerGone`] **before this returns**, on the caller's
    /// thread, so the id it hands back is never one that reports nothing: a
    /// CLI blocking in [`shutdown`](RenderQueue::shutdown) or a panel waiting
    /// on a progress bar would otherwise wait for an event that cannot come.
    pub fn submit(&mut self, job: RenderJob) -> RenderJobId {
        self.next_id += 1;
        let id = RenderJobId(self.next_id);
        let total_frames = job.frame_count();
        // Registered before it is queued, so a cancellation that arrives
        // between the two is still recorded rather than discarded as
        // belonging to an unknown job.
        self.cancel_state.lock().expect("cancel state").register(id);
        let queued = match &self.tx {
            Some(tx) => tx.send((id, job)).is_ok(),
            None => false,
        };
        if !queued {
            // Nothing will ever retire this one, so retire it here — the
            // whole point of the live set is that it tracks jobs that can
            // still finish.
            self.cancel_state.lock().expect("cancel state").retire(id);
            // Started first, so a consumer that builds its row from that
            // event still has one to attach the failure to.
            (self.on_event)(RenderEvent::Started {
                job: id,
                total_frames,
            });
            (self.on_event)(RenderEvent::Failed {
                job: id,
                error: RenderError::WorkerGone,
            });
        }
        id
    }

    /// Ask a job to stop.
    ///
    /// A job still queued stops before it compiles its composition or opens
    /// its output; a running one stops at the next frame boundary. Either way
    /// the partial output is removed and the job reports
    /// [`RenderEvent::Cancelled`].
    ///
    /// A request for a job that has already terminated — the click that lands
    /// just as the render ends — is discarded rather than remembered.
    pub fn cancel(&self, job: RenderJobId) {
        self.cancel_state.lock().expect("cancel state").request(job);
    }

    /// Outstanding jobs and outstanding cancellation requests.
    ///
    /// Exists for the test that pins the bound on the cancellation state:
    /// a request for a job that has already terminated has to be discarded,
    /// and nothing else can observe that.
    #[cfg(test)]
    fn cancel_state_sizes(&self) -> (usize, usize) {
        self.cancel_state.lock().expect("cancel state").sizes()
    }

    /// Stop accepting jobs and wait for every queued one to finish.
    ///
    /// The blocking counterpart of dropping the queue, for a caller — the CLI
    /// above all — that must not exit before the output is on disk. Both
    /// close the channel and let the worker drain what was already submitted;
    /// only this one waits for it.
    pub fn shutdown(mut self) {
        drop(self.tx.take());
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::error!("render worker thread panicked");
        }
    }
}

/// Dropping the queue **does not abandon the jobs already in it.** Closing the
/// channel only stops new submissions: `recv` keeps yielding what was queued
/// before it disconnects, so the worker renders every submitted job and then
/// exits. What dropping gives up is the *waiting* — deliberately, because a
/// drop can happen on the UI thread and joining there would block it for a
/// whole render. [`RenderQueue::shutdown`] is the same thing plus the join.
///
/// Whether a discarded queue should instead cancel what it has not started is
/// a question for the export UI (`EXPORT-5`), which is the first caller that
/// can have an opinion; until then a caller that wants that calls
/// [`RenderQueue::cancel`] before dropping.
impl Drop for RenderQueue {
    fn drop(&mut self) {
        drop(self.tx.take());
        drop(self.worker.take());
    }
}

// ===========================================================================
// Worker body
// ===========================================================================

/// How the frame loop ended, short of failing.
enum FrameLoop {
    Completed(u64),
    Cancelled(u64),
}

fn is_cancelled(state: &Mutex<CancelState>, job: RenderJobId) -> bool {
    state.lock().expect("cancel state").is_requested(job)
}

/// Run one job to its terminal event.
fn run_job<H: EvalWorkerHooks>(
    evaluator: &mut Evaluator,
    hooks: &mut H,
    id: RenderJobId,
    job: RenderJob,
    cancelled: &Mutex<CancelState>,
    on_event: &dyn Fn(RenderEvent),
) -> RenderEvent {
    let RenderJob {
        document,
        comp: comp_id,
        range,
        mut encoder,
        output,
        overwrite,
    } = job;

    let total_frames = range.end.saturating_sub(range.start);

    /// Nothing has been created yet, so a cancellation here needs no cleanup.
    /// The encoder is dropped un-begun, which its own `Drop` treats as a
    /// no-op.
    macro_rules! bail_if_cancelled {
        () => {
            if is_cancelled(cancelled, id) {
                return RenderEvent::Cancelled {
                    job: id,
                    frames_rendered: 0,
                };
            }
        };
    }

    // A job cancelled while it sat in the queue must not compile a
    // composition, register processors, or create an output directory on its
    // way to noticing. Cancellation outranks the precondition checks too: a
    // job the user has given up on should not come back as a conflict.
    bail_if_cancelled!();

    // --- everything that can be decided without touching the evaluator -----
    if let Err(error) = check_preconditions(&document, comp_id, &range, &output, overwrite) {
        // Nothing was begun, so there is nothing to abort: the encoder is
        // dropped in `Ready` and its own `Drop` is a no-op there.
        return RenderEvent::Failed { job: id, error };
    }
    let comp = document
        .get_composition(comp_id)
        .expect("checked by check_preconditions")
        .clone();

    let compiled = match compile_composition(&comp, Graph::new()) {
        Ok(compiled) => compiled,
        Err(error) => {
            return RenderEvent::Failed {
                job: id,
                error: error.into(),
            };
        }
    };

    // --- a private evaluator, rebuilt for this job -------------------------
    // `reset` rather than a fresh `Evaluator` so the cache budget the queue
    // was spawned with survives, exactly as it does for a structural resync
    // of the interactive service.
    evaluator.reset();
    hooks.sync(
        &mut ProcessorSync::new(evaluator),
        &compiled.graph,
        Some(&document),
        &InvalidationHint::Structural,
    );
    // After `sync`, which the reset above would otherwise undo.
    evaluator.set_document(document.clone());

    // Compiling and registering processors can take a while on a large
    // document, so ask again before the first thing that touches the
    // filesystem.
    bail_if_cancelled!();

    if let Err(e) = encoder.begin() {
        // A failed `begin` may still have got partway — directories made, a
        // header written — and the trait puts that cleanup here rather than
        // in `Drop`, which an encoder that never reached its active state has
        // no reason to run.
        abort(encoder.as_mut(), id);
        return RenderEvent::Failed {
            job: id,
            error: e.into(),
        };
    }

    let outcome = render_frames(
        evaluator,
        hooks,
        id,
        &comp,
        &compiled,
        range,
        total_frames,
        encoder.as_mut(),
        cancelled,
        on_event,
    );

    match outcome {
        Ok(FrameLoop::Completed(frames)) => match encoder.finish() {
            Ok(()) => RenderEvent::Completed { job: id, frames },
            // A failed close leaves the output unfinished, and the trait
            // guarantees the encoder is still abortable — a container that
            // dies partway through its trailer is the case this exists for.
            // Relying on `Drop` instead would assume every implementation
            // tracks that it failed, which the contract does not require.
            Err(e) => {
                abort(encoder.as_mut(), id);
                RenderEvent::Failed {
                    job: id,
                    error: e.into(),
                }
            }
        },
        Ok(FrameLoop::Cancelled(frames_rendered)) => {
            abort(encoder.as_mut(), id);
            RenderEvent::Cancelled {
                job: id,
                frames_rendered,
            }
        }
        Err(error) => {
            abort(encoder.as_mut(), id);
            RenderEvent::Failed { job: id, error }
        }
    }
}

/// Everything a job can be refused for before a frame is evaluated.
fn check_preconditions(
    document: &Document,
    comp: CompId,
    range: &Range<u64>,
    output: &RenderOutput,
    overwrite: OverwritePolicy,
) -> Result<(), RenderError> {
    if document.get_composition(comp).is_none() {
        return Err(RenderError::CompositionNotFound(comp));
    }
    if range.end <= range.start {
        return Err(RenderError::EmptyRange {
            start: range.start,
            end: range.end,
        });
    }
    if overwrite == OverwritePolicy::Refuse {
        let conflicts = output.conflicts(range.clone());
        if !conflicts.is_empty() {
            let total = conflicts.len();
            let mut sample = conflicts;
            sample.truncate(CONFLICT_SAMPLE);
            return Err(RenderError::OutputExists { sample, total });
        }
    }
    Ok(())
}

/// Whether anything at all occupies `path`.
///
/// `symlink_metadata` rather than `exists`, so a symlink pointing nowhere
/// still counts: the encoder creates its frames with `create_new`, which such
/// a link makes fail, and a job that would die on frame 7 should be refused
/// at frame 0 instead.
///
/// Public because a render's output is not only its frames — `ravel-cli` puts
/// a WAV beside them ([`RenderOutput::conflicts`] does not know about it) and
/// has to ask the same question about it. A front end that asks a weaker one
/// declares a dangling link free and then writes through it, which is how a
/// render comes to truncate a file outside its own output directory.
pub fn occupied(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

#[allow(clippy::too_many_arguments)]
fn render_frames<H: EvalWorkerHooks>(
    evaluator: &mut Evaluator,
    hooks: &mut H,
    id: RenderJobId,
    comp: &crate::composition::Composition,
    compiled: &crate::composition::compile::CompilationResult,
    range: Range<u64>,
    total_frames: u64,
    encoder: &mut dyn Encoder,
    cancelled: &Mutex<CancelState>,
    on_event: &dyn Fn(RenderEvent),
) -> Result<FrameLoop, RenderError> {
    let mut rendered = 0u64;
    for frame in range {
        // Frame boundary: the only place inside the loop a cancellation
        // takes effect, so a half-encoded frame is not a state the worker
        // can be stopped in.
        if is_cancelled(cancelled, id) {
            return Ok(FrameLoop::Cancelled(rendered));
        }

        // A render declares both cache axes it depends on. Both happen to be
        // the defaults today; stating them here means a future change to
        // those defaults cannot quietly downgrade an export, which is the
        // whole point of the axes existing (`cache-plan.md`,
        // `motion-blur-plan.md`).
        let ctx = EvalContext::new(frame, comp.frame_rate, comp.resolution)
            .with_comp_resolution(comp.resolution)
            .with_quality(Quality::Final)
            .with_min_precision(Precision::F32);

        let value = evaluator
            .evaluate_at(&[], &compiled.graph, compiled.output_node, &ctx)
            .map_err(|source| RenderError::Eval { frame, source })?;
        let value = hooks.finalize(value, &ctx);
        let picture = value
            .downcast_ref::<FrameBuffer>()
            .ok_or(RenderError::NotAFrame { frame })?;

        encoder.write_frame(picture, frame)?;
        rendered += 1;
        on_event(RenderEvent::Progress {
            job: id,
            frame,
            rendered,
            total_frames,
        });
    }
    Ok(FrameLoop::Completed(rendered))
}

/// Abandon the output, logging rather than propagating a cleanup failure:
/// the job is already lost, and the caller's error is the interesting one.
fn abort(encoder: &mut dyn Encoder, id: RenderJobId) {
    if let Err(e) = encoder.abort() {
        tracing::warn!(job = id.raw(), error = %e, "removing partial render output failed");
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::{Composition, Layer};
    use crate::eval::{EvalScope, NodeProcessor, ProcessorRegistry as _, ResolvedParams};
    use crate::graph::Node;
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
    use crate::media::MediaResult;
    use crate::media::encode::{PngDepth, SequenceCodec};
    use crate::network;
    use crate::runtime::{EvalRequest, EvalService};
    use crate::types::{Color, FrameRate, NodeData};
    use anyhow::Context as _;
    use crossbeam_channel::{Receiver, Sender, unbounded};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };
    const RES: (u32, u32) = (4, 4);
    fn comp_id() -> CompId {
        CompId::new(1)
    }
    const TIMEOUT: Duration = Duration::from_secs(10);

    // ---- document fixture -------------------------------------------------

    /// A one-layer composition whose background colour carries `marker`.
    ///
    /// The marker rides on the *document* rather than on the hooks, because
    /// the snapshot test needs a value that only the submitted document can
    /// supply: [`FrameSource`] reads it back out of the document the
    /// evaluator was given.
    fn document_with(marker: f32) -> Arc<Document> {
        let source = NodeId::new(1);
        let out = NodeId::new(2);
        let network = Graph::new()
            .add_node(Node::new(source, "test.frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .expect("source node")
            .add_node(
                Node::new(out, network::NET_OUT_TYPE_KEY)
                    .with_input(network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .expect("out node")
            .add_edge(
                EdgeId::new(1),
                source,
                OutputPortIndex(0),
                out,
                InputPortIndex(0),
            )
            .expect("network edge");

        let layer = Layer::new(LayerId::new(1), "layer", network).with_time(0, 0, 1_000);
        let mut comp = Composition::new(comp_id(), "comp", RES, FPS, 1_000);
        comp.background_color = Color::new(marker, 0.0, 0.0, 1.0);
        comp.layers.push_back(layer);
        Arc::new(Document::new(Graph::new()).with_composition(comp))
    }

    // ---- stub processor and hooks ----------------------------------------

    /// Emits a frame whose every channel is `document marker + frame number`.
    ///
    /// Reading the marker back through [`EvalScope::document`] is what makes
    /// the output attributable to one document rather than to the hooks, and
    /// the frame term is what makes two frames of one job distinguishable.
    ///
    /// Time-dependent on purpose: a render pulls one picture per frame, and a
    /// processor claiming otherwise would be served one cached value for the
    /// whole range.
    struct FrameSource {
        processed: Arc<AtomicUsize>,
        gate: Option<Receiver<()>>,
        fail: bool,
    }

    impl NodeProcessor for FrameSource {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.processed.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.recv_timeout(TIMEOUT).expect("gate closed");
            }
            anyhow::ensure!(!self.fail, "this processor always fails");
            let document = scope.document().context("no document")?;
            let comp = document
                .get_composition(comp_id())
                .context("composition missing")?;
            let pixel = comp.background_color.r + ctx.frame as f32;
            Ok(Arc::new(FrameBuffer::from_f32(
                ctx.resolution.0,
                ctx.resolution.1,
                vec![pixel; (ctx.resolution.0 * ctx.resolution.1 * 4) as usize],
            )))
        }

        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    /// Registers a [`FrameSource`] for every node of the compiled shell and
    /// of every layer network — the shape `GpuEvalHooks` has in the
    /// application — and records the contexts it is asked to finalize, which
    /// is how the quality and precision assertions read what the worker
    /// requested.
    struct StubHooks {
        processed: Arc<AtomicUsize>,
        gate: Option<Receiver<()>>,
        fail: bool,
        /// Panics out of `sync`, which unwinds the worker thread — the only
        /// way a submission can fail.
        kill_worker: bool,
        contexts: Arc<Mutex<Vec<EvalContext>>>,
        /// How many times `sync` ran — one per job that got as far as
        /// registering processors.
        syncs: Arc<AtomicUsize>,
        /// Signalled as `sync` is entered, so a test can cancel a job while
        /// the worker is between picking it up and opening its output.
        sync_entered: Option<Sender<()>>,
        /// Held until the test lets `sync` return.
        sync_gate: Option<Receiver<()>>,
    }

    impl StubHooks {
        fn new() -> Self {
            Self {
                processed: Arc::new(AtomicUsize::new(0)),
                gate: None,
                fail: false,
                kill_worker: false,
                contexts: Arc::new(Mutex::new(Vec::new())),
                syncs: Arc::new(AtomicUsize::new(0)),
                sync_entered: None,
                sync_gate: None,
            }
        }
    }

    impl EvalWorkerHooks for StubHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            self.syncs.fetch_add(1, Ordering::SeqCst);
            assert!(!self.kill_worker, "deliberately killing the render worker");
            if let Some(entered) = &self.sync_entered {
                let _ = entered.send(());
            }
            if let Some(gate) = &self.sync_gate {
                gate.recv_timeout(TIMEOUT).expect("sync gate closed");
            }
            if !matches!(hint, InvalidationHint::Structural) {
                return;
            }
            let mut ids: Vec<NodeId> = graph.nodes().map(|node| node.id).collect();
            if let Some(document) = document {
                for comp in document.compositions.values() {
                    for layer in &comp.layers {
                        ids.extend(layer.network.nodes().map(|node| node.id));
                    }
                }
            }
            for id in ids {
                evaluator.register(
                    id,
                    Arc::new(FrameSource {
                        processed: self.processed.clone(),
                        gate: self.gate.clone(),
                        fail: self.fail,
                    }),
                );
            }
        }

        fn finalize(&mut self, value: Arc<dyn NodeData>, ctx: &EvalContext) -> Arc<dyn NodeData> {
            self.contexts.lock().expect("contexts").push(*ctx);
            value
        }
    }

    // ---- recording encoder ------------------------------------------------

    /// An encoder that writes real files, so the conflict check and the
    /// cleanup run against a filesystem rather than a model of one.
    /// Deliberately simpler than `ImageSequenceEncoder`: what it adds is a
    /// record of the call order, which is what the worker's half of the
    /// contract is about.
    /// Where an encoder should fail, so each terminator's cleanup path can be
    /// exercised. A container that dies while writing its trailer is the
    /// `Finish` case, which no encoder in the tree can produce yet.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    enum FailAt {
        #[default]
        Nothing,
        Begin,
        Write(u64),
        Finish,
    }

    struct RecordingEncoder {
        output: ImageSequenceOutput,
        log: Arc<Mutex<Vec<String>>>,
        created: Vec<PathBuf>,
        /// Frame number and the value of its first pixel.
        frames: Arc<Mutex<Vec<(u64, f32)>>>,
        fail_at: FailAt,
    }

    impl RecordingEncoder {
        fn new(output: ImageSequenceOutput) -> Self {
            Self {
                output,
                log: Arc::new(Mutex::new(Vec::new())),
                created: Vec::new(),
                frames: Arc::new(Mutex::new(Vec::new())),
                fail_at: FailAt::Nothing,
            }
        }
    }

    impl Encoder for RecordingEncoder {
        fn begin(&mut self) -> MediaResult<()> {
            self.log.lock().expect("log").push("begin".into());
            // The directory is made before the failure on purpose: a `begin`
            // that fails partway is exactly the case the worker's cleanup
            // call exists for.
            std::fs::create_dir_all(self.output.directory())?;
            if self.fail_at == FailAt::Begin {
                return Err(MediaError::EncodeError("begin refused".into()));
            }
            Ok(())
        }

        fn write_frame(&mut self, frame: &FrameBuffer, index: u64) -> MediaResult<()> {
            self.log.lock().expect("log").push(format!("write {index}"));
            if self.fail_at == FailAt::Write(index) {
                return Err(MediaError::EncodeError(format!("frame {index} refused")));
            }
            let path = self.output.frame_path(index);
            let existed = path.exists();
            let pixel = frame.as_f32()[0];
            std::fs::write(&path, pixel.to_le_bytes())?;
            // Like `ImageSequenceEncoder`: a frame that replaced one the user
            // already had is not this encoder's to delete.
            if !existed {
                self.created.push(path);
            }
            self.frames.lock().expect("frames").push((index, pixel));
            Ok(())
        }

        fn finish(&mut self) -> MediaResult<()> {
            self.log.lock().expect("log").push("finish".into());
            if self.fail_at == FailAt::Finish {
                // Keeps `created`, which is what the trait promises: the
                // output is not final, so it is still this encoder's to
                // remove when the worker aborts.
                return Err(MediaError::EncodeError("finish refused".into()));
            }
            self.created.clear();
            Ok(())
        }

        fn abort(&mut self) -> MediaResult<()> {
            self.log.lock().expect("log").push("abort".into());
            crate::media::encode::remove_partial_output(std::mem::take(&mut self.created))
        }
    }

    // ---- harness ----------------------------------------------------------

    fn sequence_output(dir: &Path) -> ImageSequenceOutput {
        ImageSequenceOutput::new(dir, "frame_", "", SequenceCodec::Png(PngDepth::Eight), 4)
            .expect("fixed test name is valid")
    }

    /// What a submitted job leaves the test to inspect.
    struct Submitted {
        job: RenderJob,
        frames: Arc<Mutex<Vec<(u64, f32)>>>,
        log: Arc<Mutex<Vec<String>>>,
    }

    fn job(dir: &Path, document: Arc<Document>, range: Range<u64>) -> Submitted {
        failing_job(dir, document, range, FailAt::Nothing)
    }

    fn failing_job(
        dir: &Path,
        document: Arc<Document>,
        range: Range<u64>,
        fail_at: FailAt,
    ) -> Submitted {
        let output = sequence_output(dir);
        let mut encoder = RecordingEncoder::new(output.clone());
        encoder.fail_at = fail_at;
        let frames = encoder.frames.clone();
        let log = encoder.log.clone();
        Submitted {
            job: RenderJob::new(
                document,
                comp_id(),
                range,
                Box::new(encoder),
                RenderOutput::Sequence(output),
            ),
            frames,
            log,
        }
    }

    struct Harness {
        queue: RenderQueue,
        events: Receiver<RenderEvent>,
        processed: Arc<AtomicUsize>,
        syncs: Arc<AtomicUsize>,
        contexts: Arc<Mutex<Vec<EvalContext>>>,
    }

    impl Harness {
        /// Drain events until the terminal one for `job`.
        fn terminal(&self, job: RenderJobId) -> RenderEvent {
            loop {
                let event = self.events.recv_timeout(TIMEOUT).expect("render event");
                if event.job() == job && event.is_terminal() {
                    return event;
                }
            }
        }
    }

    fn spawn(hooks: StubHooks) -> Harness {
        let processed = hooks.processed.clone();
        let syncs = hooks.syncs.clone();
        let contexts = hooks.contexts.clone();
        let (tx, events) = unbounded();
        let queue = RenderQueue::spawn(hooks, move |event| {
            let _ = tx.send(event);
        });
        Harness {
            queue,
            events,
            processed,
            syncs,
            contexts,
        }
    }

    /// A private directory per test, removed first so a previous run's output
    /// cannot make a conflict test pass for the wrong reason.
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ravel-render-worker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn file_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).expect("output directory").count()
    }

    // ---- tests ------------------------------------------------------------

    /// The plan's first completion criterion: ten frames in, ten files out,
    /// named by absolute frame number, encoder driven begin → write\* →
    /// finish.
    #[test]
    fn a_ten_frame_job_writes_ten_frames() {
        let dir = temp_dir("ten-frames");
        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 0..10);
        let (frames, log) = (submitted.frames.clone(), submitted.log.clone());
        let id = h.queue.submit(submitted.job);

        match h.terminal(id) {
            RenderEvent::Completed { frames, .. } => assert_eq!(frames, 10),
            other => panic!("expected completion, got {other:?}"),
        }

        let written: Vec<u64> = frames
            .lock()
            .expect("frames")
            .iter()
            .map(|(index, _)| *index)
            .collect();
        assert_eq!(written, (0..10).collect::<Vec<_>>(), "ascending, one each");
        assert_eq!(file_count(&dir), 10, "one file per frame");
        assert!(dir.join("frame_0009.png").exists());

        let log = log.lock().expect("log").clone();
        assert_eq!(log.first().map(String::as_str), Some("begin"));
        assert_eq!(log.last().map(String::as_str), Some("finish"));
        assert!(!log.contains(&"abort".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// File names come from the absolute frame number, which is what lets a
    /// range be split across processes.
    #[test]
    fn a_range_renders_the_frames_it_names() {
        let dir = temp_dir("absolute-range");
        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 100..103);
        let frames = submitted.frames.clone();
        let id = h.queue.submit(submitted.job);
        assert!(matches!(h.terminal(id), RenderEvent::Completed { .. }));

        let written: Vec<u64> = frames
            .lock()
            .expect("frames")
            .iter()
            .map(|(index, _)| *index)
            .collect();
        assert_eq!(written, vec![100, 101, 102]);
        assert!(dir.join("frame_0100.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The unit's own criterion and the homework `BLUR-3` could not do
    /// without a worker to check it: an export declares `Quality::Final` and
    /// `Precision::F32` for **every** frame, so it can never be served a
    /// preview-grade or reduced-precision cache entry.
    #[test]
    fn every_frame_is_requested_at_final_quality_and_full_precision() {
        let dir = temp_dir("final-quality");
        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 0..4);
        let id = h.queue.submit(submitted.job);
        assert!(matches!(h.terminal(id), RenderEvent::Completed { .. }));

        let contexts = h.contexts.lock().expect("contexts").clone();
        assert_eq!(contexts.len(), 4, "one finalize per frame");
        for ctx in &contexts {
            assert_eq!(ctx.quality, Quality::Final, "an export must not be Preview");
            assert_eq!(ctx.min_precision, Precision::F32);
            assert_eq!(ctx.resolution, RES, "the comp resolution, unscaled");
            assert_eq!(ctx.comp_resolution, RES);
            assert_eq!(ctx.fps, FPS);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A job renders the document it was handed, not the one the submitter
    /// went on to edit.
    #[test]
    fn a_job_renders_the_document_it_was_submitted_with() {
        let dir_a = temp_dir("snapshot-a");
        let dir_b = temp_dir("snapshot-b");
        let (gate_tx, gate_rx) = unbounded();
        let mut hooks = StubHooks::new();
        hooks.gate = Some(gate_rx);
        let mut h = spawn(hooks);

        // The submitter's document at submission time.
        let mut document = document_with(0.0);
        let first = job(&dir_a, document.clone(), 0..2);
        let frames_a = first.frames.clone();
        let job_a = h.queue.submit(first.job);

        // Edit it while the first render is in flight. `Document` is
        // immutable-by-clone, so this is what a UI edit amounts to.
        document = document_with(1_000.0);
        let second = job(&dir_b, document, 0..2);
        let frames_b = second.frames.clone();
        let job_b = h.queue.submit(second.job);

        // Release the gate generously: the shell chain pulls several nodes
        // per frame and the exact count is not what is under test.
        for _ in 0..256 {
            let _ = gate_tx.send(());
        }
        assert!(matches!(h.terminal(job_a), RenderEvent::Completed { .. }));
        assert!(matches!(h.terminal(job_b), RenderEvent::Completed { .. }));

        assert_eq!(
            frames_a.lock().expect("frames").clone(),
            vec![(0, 0.0), (1, 1.0)],
            "the first job must not see an edit made after it was submitted",
        );
        assert_eq!(
            frames_b.lock().expect("frames").clone(),
            vec![(0, 1_000.0), (1, 1_001.0)],
            "the second job must see the edit it was submitted with",
        );
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// Cancellation lands on a frame boundary and takes the partial output
    /// with it.
    #[test]
    fn cancelling_stops_at_a_frame_boundary_and_removes_partial_output() {
        let dir = temp_dir("cancel");
        let (gate_tx, gate_rx) = unbounded();
        let mut hooks = StubHooks::new();
        hooks.gate = Some(gate_rx);
        let mut h = spawn(hooks);
        let submitted = job(&dir, document_with(0.0), 0..50);
        let (frames, log) = (submitted.frames.clone(), submitted.log.clone());
        let id = h.queue.submit(submitted.job);

        // Feed the gate until the first frame is written, then cancel and
        // keep feeding so the worker is never stuck inside `process`.
        loop {
            match h.events.recv_timeout(Duration::from_millis(20)) {
                Ok(RenderEvent::Progress { rendered, .. }) => {
                    assert_eq!(rendered, 1);
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = gate_tx.send(());
                }
            }
        }
        h.queue.cancel(id);
        for _ in 0..1024 {
            let _ = gate_tx.send(());
        }

        let rendered = match h.terminal(id) {
            RenderEvent::Cancelled {
                frames_rendered, ..
            } => frames_rendered,
            other => panic!("expected cancellation, got {other:?}"),
        };
        assert!(
            (1..50).contains(&rendered),
            "the job ran to the end instead of stopping: {rendered}",
        );
        assert_eq!(
            frames.lock().expect("frames").len(),
            rendered as usize,
            "the reported count must match the frames actually written",
        );
        assert!(
            log.lock().expect("log").contains(&"abort".to_string()),
            "a cancelled job must abandon its output",
        );
        assert_eq!(
            file_count(&dir),
            0,
            "partial output must not survive a cancellation",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failing job is reported as failed, drops its partial output, and
    /// does not take the queue down with it.
    #[test]
    fn a_failing_job_does_not_stop_the_queue() {
        let dir_bad = temp_dir("fail-bad");
        let dir_next = temp_dir("fail-next");
        let mut hooks = StubHooks::new();
        hooks.fail = true;
        let mut h = spawn(hooks);

        let bad = job(&dir_bad, document_with(0.0), 0..5);
        let bad_log = bad.log.clone();
        let bad_id = h.queue.submit(bad.job);
        match h.terminal(bad_id) {
            RenderEvent::Failed {
                error: RenderError::Eval { frame, .. },
                ..
            } => assert_eq!(frame, 0, "the first frame is where it broke"),
            other => panic!("expected an evaluation failure, got {other:?}"),
        }
        assert!(
            bad_log.lock().expect("log").contains(&"abort".to_string()),
            "a failed job must abandon its output too",
        );
        assert_eq!(file_count(&dir_bad), 0);

        // The worker survived: a later job still reaches the encoder. These
        // hooks keep failing, so what this asserts is that the job ran at
        // all, not that it succeeded.
        let next = job(&dir_next, document_with(0.0), 0..1);
        let next_log = next.log.clone();
        let next_id = h.queue.submit(next.job);
        assert!(matches!(h.terminal(next_id), RenderEvent::Failed { .. }));
        assert!(
            next_log.lock().expect("log").contains(&"begin".to_string()),
            "the queue stopped after the failing job",
        );
        let _ = std::fs::remove_dir_all(&dir_bad);
        let _ = std::fs::remove_dir_all(&dir_next);
    }

    /// A job whose output is already on disk is refused **before the first
    /// evaluation**: no processor runs and the encoder is never begun.
    #[test]
    fn existing_output_is_refused_without_evaluating_a_frame() {
        let dir = temp_dir("conflict");
        // A previous render. Only one of these two frames is in the range
        // about to be submitted, which is what makes this a name-level check
        // rather than a "the directory is not empty" one.
        std::fs::write(dir.join("frame_0003.png"), b"previous render").unwrap();
        std::fs::write(dir.join("frame_0099.png"), b"previous render").unwrap();

        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 0..10);
        let (frames, log) = (submitted.frames.clone(), submitted.log.clone());
        let id = h.queue.submit(submitted.job);

        match h.terminal(id) {
            RenderEvent::Failed {
                error: RenderError::OutputExists { sample, total },
                ..
            } => {
                assert_eq!(total, 1, "only the frame inside the range conflicts");
                assert_eq!(sample, vec![dir.join("frame_0003.png")]);
            }
            other => panic!("expected a conflict refusal, got {other:?}"),
        }
        assert_eq!(
            h.processed.load(Ordering::SeqCst),
            0,
            "the job must be refused before a single frame is evaluated",
        );
        assert!(
            log.lock().expect("log").is_empty(),
            "the encoder must not even be begun",
        );
        assert!(frames.lock().expect("frames").is_empty());
        assert_eq!(
            std::fs::read(dir.join("frame_0003.png")).unwrap(),
            b"previous render",
            "the refusal must leave the existing frame exactly as it was",
        );
        assert_eq!(file_count(&dir), 2, "nothing was added either");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same job goes through once the submitter says the existing output
    /// is theirs to replace.
    #[test]
    fn an_explicit_overwrite_lets_the_same_job_through() {
        let dir = temp_dir("overwrite");
        std::fs::write(dir.join("frame_0003.png"), b"previous render").unwrap();

        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(7.0), 0..10);
        let frames = submitted.frames.clone();
        let id = h
            .queue
            .submit(submitted.job.with_overwrite(OverwritePolicy::Replace));

        match h.terminal(id) {
            RenderEvent::Completed { frames, .. } => assert_eq!(frames, 10),
            other => panic!("expected completion, got {other:?}"),
        }
        assert_eq!(frames.lock().expect("frames").len(), 10);
        assert_ne!(
            std::fs::read(dir.join("frame_0003.png")).unwrap(),
            b"previous render",
            "the conflicting frame must have been replaced",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The conflict check is per file name, so the plan's `--range` split —
    /// several runs writing disjoint ranges into one directory — is not
    /// mistaken for a collision.
    #[test]
    fn disjoint_ranges_share_an_output_directory() {
        let dir = temp_dir("split-range");
        let mut h = spawn(StubHooks::new());

        let first = job(&dir, document_with(0.0), 0..5);
        let first_id = h.queue.submit(first.job);
        assert!(matches!(
            h.terminal(first_id),
            RenderEvent::Completed { .. }
        ));

        // The second half, submitted when the first half is already on disk.
        let second = job(&dir, document_with(0.0), 5..10);
        let second_id = h.queue.submit(second.job);
        match h.terminal(second_id) {
            RenderEvent::Completed { frames, .. } => assert_eq!(frames, 5),
            other => panic!("a disjoint range must not conflict, got {other:?}"),
        }
        assert_eq!(file_count(&dir), 10, "the two halves make one sequence");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The separation this worker exists for: a render neither rides the
    /// interactive service's cache nor disturbs it.
    #[test]
    fn rendering_leaves_the_interactive_cache_alone() {
        let dir = temp_dir("cache-separation");
        let processed = Arc::new(AtomicUsize::new(0));

        // Two hooks instances sharing only the counter — the arrangement the
        // application has, where the two workers share a `GpuContext` and
        // nothing else.
        let mut service_hooks = StubHooks::new();
        service_hooks.processed = processed.clone();
        let (update_tx, updates) = unbounded();
        let mut service = EvalService::spawn(service_hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let document = document_with(0.0);
        let comp = document.get_composition(comp_id()).expect("comp").clone();
        let compiled = compile_composition(&comp, Graph::new()).expect("compile");
        let request = |frame: u64| EvalRequest {
            graph: compiled.graph.clone(),
            nodes: vec![compiled.output_node],
            path: Vec::new(),
            ctx: EvalContext::new(frame, FPS, RES).with_quality(Quality::Preview),
            document: Some(document.clone()),
            hint: InvalidationHint::None,
        };

        service.request(request(0));
        updates.recv_timeout(TIMEOUT).expect("interactive update");
        let interactive = processed.load(Ordering::SeqCst);
        assert!(interactive > 0, "the interactive pull did no work");

        let mut render_hooks = StubHooks::new();
        render_hooks.processed = processed.clone();
        let mut h = spawn(render_hooks);
        let submitted = job(&dir, document.clone(), 0..3);
        let id = h.queue.submit(submitted.job);
        assert!(matches!(h.terminal(id), RenderEvent::Completed { .. }));

        let after_render = processed.load(Ordering::SeqCst);
        assert_eq!(
            after_render - interactive,
            interactive * 3,
            "the render must do its own work for every frame rather than \
             being served the interactive cache",
        );

        // And the interactive cache still answers what it already held.
        service.request(request(0));
        updates.recv_timeout(TIMEOUT).expect("second update");
        assert_eq!(
            processed.load(Ordering::SeqCst),
            after_render,
            "the render dirtied or evicted the interactive cache",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A composition the document does not hold fails the job instead of
    /// panicking the worker.
    #[test]
    fn an_unknown_composition_fails_the_job() {
        let dir = temp_dir("unknown-comp");
        let mut h = spawn(StubHooks::new());
        let output = sequence_output(&dir);
        let id = h.queue.submit(RenderJob::new(
            document_with(0.0),
            CompId::new(99),
            0..2,
            Box::new(RecordingEncoder::new(output.clone())),
            RenderOutput::Sequence(output),
        ));
        assert!(matches!(
            h.terminal(id),
            RenderEvent::Failed {
                error: RenderError::CompositionNotFound(_),
                ..
            }
        ));
        assert_eq!(h.processed.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty range is a caller mistake, not a job that trivially wins.
    #[test]
    fn an_empty_range_fails_the_job() {
        let dir = temp_dir("empty-range");
        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 5..5);
        let log = submitted.log.clone();
        let id = h.queue.submit(submitted.job);
        assert!(matches!(
            h.terminal(id),
            RenderEvent::Failed {
                error: RenderError::EmptyRange { start: 5, end: 5 },
                ..
            }
        ));
        assert!(log.lock().expect("log").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Progress covers the range frame by frame and agrees with the terminal
    /// event — the readout a queue panel and the CLI both draw from.
    #[test]
    fn progress_is_reported_frame_by_frame() {
        let dir = temp_dir("progress");
        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 0..3);
        let id = h.queue.submit(submitted.job);

        let mut started = false;
        let mut seen = Vec::new();
        loop {
            let event = h.events.recv_timeout(TIMEOUT).expect("event");
            assert_eq!(event.job(), id);
            match event {
                RenderEvent::Started { total_frames, .. } => {
                    assert_eq!(total_frames, 3);
                    started = true;
                }
                RenderEvent::Progress {
                    frame,
                    rendered,
                    total_frames,
                    ..
                } => {
                    assert_eq!(total_frames, 3);
                    seen.push((frame, rendered));
                }
                RenderEvent::Completed { frames, .. } => {
                    assert_eq!(frames, 3);
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(started, "the job must announce itself before it reports");
        assert_eq!(seen, vec![(0, 1), (1, 2), (2, 3)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `shutdown` is the caller's way to wait — the CLI cannot exit before
    /// the output is on disk.
    #[test]
    fn shutdown_waits_for_queued_jobs() {
        let dir = temp_dir("shutdown");
        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 0..4);
        let frames = submitted.frames.clone();
        h.queue.submit(submitted.job);
        h.queue.shutdown();
        assert_eq!(
            frames.lock().expect("frames").len(),
            4,
            "shutdown returned before the job finished",
        );
        assert_eq!(file_count(&dir), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- terminator failures ---------------------------------------------

    /// A `begin` that fails may already have created part of its destination,
    /// so the worker abandons the output rather than assuming a `Drop` will.
    #[test]
    fn a_failed_begin_abandons_the_output() {
        let dir = temp_dir("begin-fails");
        let mut h = spawn(StubHooks::new());
        let submitted = failing_job(&dir, document_with(0.0), 0..5, FailAt::Begin);
        let log = submitted.log.clone();
        let id = h.queue.submit(submitted.job);

        assert!(matches!(
            h.terminal(id),
            RenderEvent::Failed {
                error: RenderError::Encode(_),
                ..
            }
        ));
        assert_eq!(
            log.lock().expect("log").clone(),
            vec!["begin".to_string(), "abort".to_string()],
            "a failed begin must be followed by the cleanup call",
        );
        assert_eq!(
            h.processed.load(Ordering::SeqCst),
            0,
            "no frame may be evaluated once the output could not be opened",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A frame the encoder refuses fails the job and takes the frames written
    /// before it with it.
    #[test]
    fn a_failed_frame_write_abandons_the_output() {
        let dir = temp_dir("write-fails");
        let mut h = spawn(StubHooks::new());
        let submitted = failing_job(&dir, document_with(0.0), 0..5, FailAt::Write(2));
        let log = submitted.log.clone();
        let id = h.queue.submit(submitted.job);

        assert!(matches!(
            h.terminal(id),
            RenderEvent::Failed {
                error: RenderError::Encode(_),
                ..
            }
        ));
        let log = log.lock().expect("log").clone();
        assert_eq!(log.last().map(String::as_str), Some("abort"));
        assert!(
            !log.contains(&"write 3".to_string()),
            "the job must stop at the frame that failed: {log:?}",
        );
        assert_eq!(
            file_count(&dir),
            0,
            "the frames written before the failure must not survive it",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `finish` that fails has not made the output final, so the worker
    /// abandons it. `ImageSequenceEncoder` cannot fail there today — its
    /// frames are already renamed into place one at a time — but a container
    /// writing a trailer can, which is why the contract says so and why this
    /// is checked with an encoder that does fail.
    #[test]
    fn a_failed_finish_abandons_the_output() {
        let dir = temp_dir("finish-fails");
        let mut h = spawn(StubHooks::new());
        let submitted = failing_job(&dir, document_with(0.0), 0..3, FailAt::Finish);
        let (frames, log) = (submitted.frames.clone(), submitted.log.clone());
        let id = h.queue.submit(submitted.job);

        assert!(matches!(
            h.terminal(id),
            RenderEvent::Failed {
                error: RenderError::Encode(_),
                ..
            }
        ));
        assert_eq!(
            frames.lock().expect("frames").len(),
            3,
            "every frame was written; it is the close that failed",
        );
        let log = log.lock().expect("log").clone();
        assert_eq!(
            log.last().map(String::as_str),
            Some("abort"),
            "a failed finish must be followed by the cleanup call: {log:?}",
        );
        assert_eq!(
            file_count(&dir),
            0,
            "output that was never closed must not be left on disk",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- cancellation reach ----------------------------------------------

    /// A job cancelled while it was still queued must not compile, register
    /// processors, or open its output on the way to noticing.
    #[test]
    fn a_job_cancelled_while_queued_never_opens_its_output() {
        let dir = temp_dir("cancel-queued");
        let pending = dir.join("pending");
        let (gate_tx, gate_rx) = unbounded();
        let mut hooks = StubHooks::new();
        hooks.gate = Some(gate_rx);
        let mut h = spawn(hooks);

        // The job in front holds the worker inside `process`, so the second
        // one is provably still in the queue when it is cancelled.
        let blocking = job(&dir, document_with(0.0), 0..2);
        let blocking_id = h.queue.submit(blocking.job);
        let queued = job(&pending, document_with(0.0), 0..5);
        let (log, frames) = (queued.log.clone(), queued.frames.clone());
        let queued_id = h.queue.submit(queued.job);

        loop {
            let event = h.events.recv_timeout(TIMEOUT).expect("event");
            if matches!(event, RenderEvent::Started { .. }) && event.job() == blocking_id {
                break;
            }
        }
        h.queue.cancel(queued_id);
        for _ in 0..256 {
            let _ = gate_tx.send(());
        }

        assert!(matches!(
            h.terminal(blocking_id),
            RenderEvent::Completed { .. }
        ));
        match h.terminal(queued_id) {
            RenderEvent::Cancelled {
                frames_rendered, ..
            } => assert_eq!(frames_rendered, 0),
            other => panic!("expected cancellation, got {other:?}"),
        }
        assert!(
            log.lock().expect("log").is_empty(),
            "a job cancelled in the queue must never reach its encoder",
        );
        assert!(frames.lock().expect("frames").is_empty());
        assert!(
            !pending.exists(),
            "opening the output would have created {}",
            pending.display(),
        );
        assert_eq!(
            h.syncs.load(Ordering::SeqCst),
            1,
            "only the job in front may have compiled and registered processors",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same, one step later: cancelled after the job was picked up but
    /// before its output was opened. Registering processors for a large
    /// document is not instant, and the cancellation has to land inside that
    /// window too.
    #[test]
    fn a_job_cancelled_during_sync_never_opens_its_output() {
        let dir = temp_dir("cancel-during-sync");
        let pending = dir.join("pending");
        let (entered_tx, entered_rx) = unbounded();
        let (gate_tx, gate_rx) = unbounded();
        let mut hooks = StubHooks::new();
        hooks.sync_entered = Some(entered_tx);
        hooks.sync_gate = Some(gate_rx);
        let mut h = spawn(hooks);

        let submitted = job(&pending, document_with(0.0), 0..5);
        let log = submitted.log.clone();
        let id = h.queue.submit(submitted.job);

        entered_rx.recv_timeout(TIMEOUT).expect("sync entered");
        h.queue.cancel(id);
        gate_tx.send(()).expect("release sync");

        match h.terminal(id) {
            RenderEvent::Cancelled {
                frames_rendered, ..
            } => assert_eq!(frames_rendered, 0),
            other => panic!("expected cancellation, got {other:?}"),
        }
        assert!(
            log.lock().expect("log").is_empty(),
            "the output must not be opened once the job is cancelled",
        );
        assert!(
            !pending.exists(),
            "opening the output would have created {}",
            pending.display(),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancellation outranks the precondition checks: a job the user gave up
    /// on comes back cancelled, not as a complaint about its output.
    #[test]
    fn a_cancelled_job_is_not_reported_as_a_conflict() {
        let dir = temp_dir("cancel-outranks-conflict");
        std::fs::write(dir.join("frame_0000.png"), b"previous render").unwrap();
        let (gate_tx, gate_rx) = unbounded();
        let mut hooks = StubHooks::new();
        hooks.gate = Some(gate_rx);
        let mut h = spawn(hooks);

        let blocking = job(&dir.join("other"), document_with(0.0), 0..2);
        let blocking_id = h.queue.submit(blocking.job);
        // Would fail with `OutputExists` if it ever reached the checks.
        let queued = job(&dir, document_with(0.0), 0..3);
        let queued_id = h.queue.submit(queued.job);

        loop {
            let event = h.events.recv_timeout(TIMEOUT).expect("event");
            if matches!(event, RenderEvent::Started { .. }) && event.job() == blocking_id {
                break;
            }
        }
        h.queue.cancel(queued_id);
        for _ in 0..256 {
            let _ = gate_tx.send(());
        }

        assert!(matches!(
            h.terminal(blocking_id),
            RenderEvent::Completed { .. }
        ));
        match h.terminal(queued_id) {
            RenderEvent::Cancelled {
                frames_rendered, ..
            } => assert_eq!(frames_rendered, 0),
            other => panic!("cancellation must outrank the conflict check, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancelling a job that has already finished — the click that lands as
    /// the render ends — must be discarded, not remembered forever.
    #[test]
    fn cancelling_a_finished_job_leaves_no_state_behind() {
        let dir = temp_dir("cancel-after-finish");
        let mut h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 0..2);
        let id = h.queue.submit(submitted.job);
        assert!(matches!(h.terminal(id), RenderEvent::Completed { .. }));

        // The terminal event is emitted after the worker retires the job, so
        // by here the id is provably gone from the live set.
        assert_eq!(h.queue.cancel_state_sizes(), (0, 0));
        h.queue.cancel(id);
        assert_eq!(
            h.queue.cancel_state_sizes(),
            (0, 0),
            "a request for a job that no longer exists must be dropped",
        );

        // Repeating it — a UI that re-sends on every click — still adds
        // nothing.
        for _ in 0..100 {
            h.queue.cancel(id);
        }
        assert_eq!(h.queue.cancel_state_sizes(), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the bound: a live job's request *is* recorded, and
    /// both sets empty out once it terminates.
    #[test]
    fn a_live_jobs_cancellation_is_recorded_and_then_retired() {
        let dir = temp_dir("cancel-live");
        let (gate_tx, gate_rx) = unbounded();
        let mut hooks = StubHooks::new();
        hooks.gate = Some(gate_rx);
        let mut h = spawn(hooks);
        let submitted = job(&dir, document_with(0.0), 0..20);
        let id = h.queue.submit(submitted.job);
        assert_eq!(
            h.queue.cancel_state_sizes(),
            (1, 0),
            "submitted, no request"
        );

        h.queue.cancel(id);
        assert_eq!(h.queue.cancel_state_sizes(), (1, 1), "request recorded");

        for _ in 0..512 {
            let _ = gate_tx.send(());
        }
        assert!(matches!(h.terminal(id), RenderEvent::Cancelled { .. }));
        assert_eq!(
            h.queue.cancel_state_sizes(),
            (0, 0),
            "terminating a job must clear both sets",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A submission the worker cannot receive is reported instead of being
    /// dropped on the floor.
    ///
    /// The only way `send` fails is a worker thread that unwound — a hook or
    /// an event callback panicked. Swallowing that left the caller holding an
    /// id that never reports anything, which is a hang for the CLI, whose
    /// whole shape is "submit, then wait for the output".
    ///
    /// The panic this test provokes prints a backtrace; that is the worker
    /// dying on purpose, not a failure.
    #[test]
    fn a_job_the_worker_cannot_receive_is_reported_as_failed() {
        let dir = temp_dir("worker-gone");
        let mut hooks = StubHooks::new();
        hooks.kill_worker = true;
        let mut h = spawn(hooks);

        // The first job kills the worker on its way through `sync`.
        let first = job(&dir, document_with(0.0), 0..1);
        h.queue.submit(first.job);

        // Submissions race the unwinding thread, so the first one or two may
        // still land in a queue nobody will read. Keep going until one is
        // refused — that is the case under test.
        let mut refused = None;
        for _ in 0..2_000 {
            let submitted = job(&dir, document_with(0.0), 0..1);
            let id = h.queue.submit(submitted.job);
            while let Ok(event) = h.events.try_recv() {
                if event.job() == id && event.is_terminal() {
                    refused = Some(event);
                }
            }
            if refused.is_some() {
                break;
            }
            std::thread::yield_now();
        }
        match refused {
            Some(RenderEvent::Failed {
                error: RenderError::WorkerGone,
                ..
            }) => {}
            Some(other) => panic!("expected WorkerGone, got {other:?}"),
            None => panic!("a job the worker never received reported nothing at all"),
        }

        // And the job is not left in the live set: the worker that would have
        // retired it is gone, so `submit` has to.
        let (live_after_refusal, _) = h.queue.cancel_state_sizes();
        for _ in 0..50 {
            let submitted = job(&dir, document_with(0.0), 0..1);
            h.queue.submit(submitted.job);
        }
        assert_eq!(
            h.queue.cancel_state_sizes().0,
            live_after_refusal,
            "refused jobs accumulate in the live set",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dropping the queue does not abandon what was already submitted: the
    /// channel disconnects, but `recv` keeps yielding the queued jobs first.
    /// The doc on `Drop` says so; this is what makes that claim checkable,
    /// and what a change of mind in `EXPORT-5` would have to update.
    #[test]
    fn dropping_the_queue_still_renders_what_was_submitted() {
        let dir = temp_dir("drop-drains");
        let h = spawn(StubHooks::new());
        let submitted = job(&dir, document_with(0.0), 0..4);
        let frames = submitted.frames.clone();
        let Harness { queue, events, .. } = h;
        let mut queue = queue;
        queue.submit(submitted.job);
        drop(queue);

        // The worker outlives the handle; the events arrive all the same.
        loop {
            match events.recv_timeout(TIMEOUT).expect("event") {
                RenderEvent::Completed { frames, .. } => {
                    assert_eq!(frames, 4);
                    break;
                }
                RenderEvent::Started { .. } | RenderEvent::Progress { .. } => {}
                other => panic!("expected the queued job to run, got {other:?}"),
            }
        }
        assert_eq!(frames.lock().expect("frames").len(), 4);
        assert_eq!(file_count(&dir), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dropping the queue must not hang, whether or not work is outstanding.
    #[test]
    fn dropping_the_queue_does_not_hang() {
        let h = spawn(StubHooks::new());
        drop(h);
    }

    // ---- JobProgress ------------------------------------------------------

    fn job_id(raw: u64) -> RenderJobId {
        RenderJobId(raw)
    }

    #[test]
    fn progress_starts_only_from_a_started_event() {
        let started = RenderEvent::Started {
            job: job_id(1),
            total_frames: 10,
        };
        let tracker = JobProgress::started(&started).expect("Started begins a tracker");
        assert_eq!(tracker.total_frames(), 10);
        assert_eq!(tracker.rendered(), 0);
        assert_eq!(tracker.state(), &JobState::Running);
        assert!(!tracker.is_finished());

        assert!(
            JobProgress::started(&RenderEvent::Completed {
                job: job_id(1),
                frames: 10,
            })
            .is_none(),
            "only Started carries the frame total"
        );
    }

    #[test]
    fn progress_folds_frames_and_reaches_a_terminal_state() {
        let mut tracker = JobProgress::started(&RenderEvent::Started {
            job: job_id(1),
            total_frames: 4,
        })
        .expect("started");

        for (rendered, frame) in (100u64..102).enumerate() {
            assert!(tracker.observe(&RenderEvent::Progress {
                job: job_id(1),
                frame,
                rendered: rendered as u64 + 1,
                total_frames: 4,
            }));
        }
        assert_eq!(tracker.rendered(), 2);
        assert_eq!(tracker.last_frame(), Some(101));
        assert!((tracker.fraction() - 0.5).abs() < f32::EPSILON);

        assert!(tracker.observe(&RenderEvent::Completed {
            job: job_id(1),
            frames: 4,
        }));
        assert_eq!(tracker.state(), &JobState::Completed);
        assert!(tracker.is_finished());
        assert!((tracker.fraction() - 1.0).abs() < f32::EPSILON);
    }

    /// A consumer hands every event to every tracker it owns, so one that is
    /// not addressed to this job must change nothing at all.
    #[test]
    fn progress_ignores_another_jobs_events() {
        let mut tracker = JobProgress::started(&RenderEvent::Started {
            job: job_id(1),
            total_frames: 4,
        })
        .expect("started");
        let before = tracker.clone();

        assert!(!tracker.observe(&RenderEvent::Progress {
            job: job_id(2),
            frame: 0,
            rendered: 1,
            total_frames: 4,
        }));
        assert_eq!(tracker, before);
    }

    /// A failure keeps the frame count it had reached and carries the error's
    /// own text, which is what a log line or a panel row needs.
    #[test]
    fn progress_records_a_failure_message() {
        let mut tracker = JobProgress::started(&RenderEvent::Started {
            job: job_id(1),
            total_frames: 4,
        })
        .expect("started");
        tracker.observe(&RenderEvent::Failed {
            job: job_id(1),
            error: RenderError::EmptyRange { start: 5, end: 5 },
        });
        let JobState::Failed { message } = tracker.state() else {
            panic!("expected a failed state, got {:?}", tracker.state());
        };
        assert!(message.contains("5..5"), "unexpected message: {message}");
        assert!(tracker.is_finished());
    }

    /// A cancelled job reports how far it got even though its output is gone.
    #[test]
    fn progress_keeps_the_reached_frame_count_after_a_cancellation() {
        let mut tracker = JobProgress::started(&RenderEvent::Started {
            job: job_id(1),
            total_frames: 8,
        })
        .expect("started");
        tracker.observe(&RenderEvent::Cancelled {
            job: job_id(1),
            frames_rendered: 3,
        });
        assert_eq!(tracker.rendered(), 3);
        assert_eq!(tracker.state(), &JobState::Cancelled);
    }
}

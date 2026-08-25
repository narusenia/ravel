// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Background evaluation service (Phase 1 of
//! `docs/implementation/done/eval-render-performance-plan.md`).
//!
//! Owns a dedicated worker thread that runs an [`Evaluator`] so graph
//! evaluation never blocks the UI thread. Requests carry a monotonically
//! increasing generation number and are **latest-wins**: when several
//! requests queue up while the worker is busy (e.g. every `Change` event of
//! a parameter scrub), the worker drains the queue and evaluates only the
//! newest one, merging the [`InvalidationHint`]s of the skipped requests so
//! no processor rebuild is lost. Coalescing is per *request*, not per target:
//! a request names one or more output nodes and either all of them are
//! evaluated or the whole request is dropped.
//!
//! Multiple targets run through the same [`Evaluator`], which is the point of
//! carrying them in one request — an inspection target upstream of the
//! composition output is a cache hit rather than a second full pull, and a
//! second service would duplicate the cache and the GPU pipeline instead.
//! The update is emitted once, after the last target: adding a target
//! therefore delays the first one's arrival by whatever the rest cost, so the
//! composition output is only as prompt as the slowest inspection target
//! riding along with it.
//!
//! The service is generic over [`EvalWorkerHooks`] so `ravel-core` stays
//! free of GPU and UI dependencies: the host supplies processor
//! registration (`sync`) and output post-processing (`finalize`, e.g.
//! rasterizing a `Geometry` for the viewer) and receives results through
//! the `on_update` callback, which is invoked on the worker thread.

use crate::cache_budget::Evicted;
use crate::composition::Document;
use crate::eval::{
    CacheIdentity, EvalContext, EvalError, Evaluator, PathSegment, ProcessorRegistry,
};
use crate::graph::Graph;
use crate::id::{CompId, NodeId};
use crate::runtime::frame_cache::SharedFrameCache;
use crate::types::NodeData;
use crossbeam_channel::{Receiver, Sender, TryRecvError, select, unbounded};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// What changed in the graph since the previous request.
///
/// Drives how [`EvalWorkerHooks::sync`] refreshes processor registrations.
/// Hints of coalesced (skipped) requests are merged, keeping the strongest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvalidationHint {
    /// Nothing changed; pull only (e.g. selection switch).
    None,
    /// Only parameters of these nodes changed; rebuilding just their
    /// processors preserves the evaluator cache for everything else.
    Params(Vec<NodeId>),
    /// Topology changed (nodes/edges added or removed, undo/redo);
    /// registrations must be rebuilt from scratch.
    Structural,
}

impl InvalidationHint {
    /// Merge with the hint of a newer request, keeping the strongest.
    /// `Structural` absorbs everything; `Params` unions node lists.
    pub fn merge(self, newer: Self) -> Self {
        use InvalidationHint::*;
        match (self, newer) {
            (Structural, _) | (_, Structural) => Structural,
            (Params(mut a), Params(b)) => {
                for id in b {
                    if !a.contains(&id) {
                        a.push(id);
                    }
                }
                Params(a)
            }
            (Params(a), None) => Params(a),
            (None, other) => other,
        }
    }
}

/// What one target of an [`EvalRequest`] evaluated to.
pub type EvalOutput = Result<Arc<dyn NodeData>, EvalError>;

/// A target that carries its own scope, evaluated alongside the request's
/// [`nodes`](EvalRequest::nodes) rather than inside their graph.
///
/// A request is one graph at one ownership path, which is exactly what a
/// composition's compiled shell chain needs — and exactly what an inspection
/// point *inside* a layer network cannot use, because a layer network is
/// evaluated recursively through its boundary node and its nodes are never in
/// the shell graph. [`Evaluator::evaluate_at`] rebinds the path and the graph
/// per call, so the worker pulls these through the **same** evaluator: what
/// the shell evaluation already computed for that network is a cache hit here
/// rather than a second pull.
#[derive(Clone)]
pub struct ScopedTarget {
    /// Ownership path the node is evaluated under (never empty in practice —
    /// the root scope is what `nodes` is for).
    pub path: Vec<PathSegment>,
    /// The graph `node` lives in: the network `path` names.
    pub graph: Graph,
    pub node: NodeId,
    /// The context this scope is evaluated under, which is **not** the
    /// request's: a layer network runs on layer-local time (REQ-LAYER-006),
    /// and the recursive shell evaluation entered it with exactly that
    /// context. Passing the request's own would both read the wrong frame and
    /// miss the cache entry the shell just filled.
    pub ctx: EvalContext,
}

/// One [`ScopedTarget`]'s outcome, tagged with the scope it was evaluated in.
///
/// The scope travels with the result because a `NodeId` alone does not
/// identify a node: two networks routinely hold the same id, and a consumer
/// that keyed results by id would hand an overlay a value from another graph.
pub struct ScopedResult {
    pub path: Vec<PathSegment>,
    pub node: NodeId,
    pub output: EvalOutput,
}

/// Result of one background evaluation, delivered via `on_update`.
pub struct EvalUpdate {
    /// Generation of the request that produced this result. Consumers must
    /// drop updates whose generation is older than the latest they issued.
    pub generation: u64,
    /// Frame the request evaluated at, so a consumer-side drop can be
    /// correlated with the worker's "eval result sent" log line.
    pub frame: u64,
    /// One (finalized) outcome per requested node, **in the order of
    /// [`EvalRequest::nodes`]** and with the same length: a target that
    /// failed contributes its `Err` rather than dropping out, so a consumer
    /// can address results positionally as well as by id.
    pub results: Vec<(NodeId, EvalOutput)>,
    /// One outcome per [`EvalRequest::scoped`] target, in the same order and
    /// with the same length. Kept apart from [`results`](Self::results) so the
    /// positional convention that field carries — target 0 is the composition
    /// output — holds however many scoped targets ride along.
    pub scoped: Vec<ScopedResult>,
    /// Per-node `process()` durations of this evaluation (cache hits are
    /// absent). Drives the node editor's load readout.
    ///
    /// Aggregated over *all* targets of the request, because the targets
    /// share one [`Evaluator`] pass: a node evaluated for the first target
    /// is normally a cache hit for the second and therefore appears once,
    /// which is exactly the cache sharing the multi-target form exists for.
    /// An eviction between targets can still process the same node twice and
    /// list it twice, so consumers must not sum by id — the node editor's
    /// readout takes the last entry.
    pub timings: Vec<(NodeId, std::time::Duration)>,
}

/// One background evaluation request (see [`EvalService::request`]).
pub struct EvalRequest {
    /// The graph the `nodes` live in (a compiled shell graph or one
    /// layer/subnet network — nested networks are pulled through the
    /// document).
    pub graph: Graph,
    /// The output nodes to pull, evaluated in order through one
    /// [`Evaluator`] so later targets reuse what earlier ones computed.
    /// A failing target does not stop the rest.
    pub nodes: Vec<NodeId>,
    /// Targets that name their own scope, pulled after `nodes` through the
    /// same [`Evaluator`] (see [`ScopedTarget`]). Empty for every caller that
    /// only wants the request's own graph.
    pub scoped: Vec<ScopedTarget>,
    /// Which composition this request evaluates, for the output-stage frame
    /// cache (`CACHE-5`).
    ///
    /// `Some` opts the request's **first** target — the composition output by
    /// the convention every caller already follows — into the frame cache,
    /// keyed by `(comp, TimeKey)`. The remaining targets are inspection
    /// points inside the graph and go through the evaluator as before.
    ///
    /// `None` opts out entirely, and so does a request with a non-empty
    /// [`path`](Self::path) or no [`document`](Self::document): a render, a
    /// benchmark and a network preview each want their own evaluation rather
    /// than the interactive cache, and the invalidation this layer relies on
    /// is a document diff it has nothing to compare without a document.
    pub comp: Option<CompId>,
    /// Ownership path the evaluation runs under; empty for the root scope.
    /// A node previewed inside a layer network passes
    /// `[PathSegment::Layer(comp, layer), ...]` so cache keys and
    /// `layer.ref` resolution match the shell-driven evaluation
    /// (REQ-LAYER-007/011).
    pub path: Vec<PathSegment>,
    pub ctx: EvalContext,
    /// Document snapshot for nested evaluation (network boundaries,
    /// `layer.ref`, media assets). Replacing it invalidates changed scopes
    /// via [`Evaluator::set_document`].
    pub document: Option<Arc<Document>>,
    pub hint: InvalidationHint,
}

/// The evaluator as a [`EvalWorkerHooks::sync`] implementation sees it:
/// processor registration and nothing else.
///
/// A hook used to receive `&mut Evaluator`, and the natural way to write a
/// structural resync was `*evaluator = Evaluator::new()`. That threw away
/// state the *service* owns — the cache budget — and since the worker
/// escalates its first request to [`InvalidationHint::Structural`], it
/// happened before the first frame was ever evaluated. The result was an
/// application whose node cache had no limit while every unit test passed.
///
/// Handing out a view instead makes that assignment a compile error, and the
/// reset it was written for now happens in the service (see
/// [`EvalService::spawn_with_budget`]) before `sync` is called at all.
pub struct ProcessorSync<'a> {
    evaluator: &'a mut Evaluator,
}

impl<'a> ProcessorSync<'a> {
    /// Lend `evaluator` to a [`EvalWorkerHooks::sync`] call.
    ///
    /// Built by whoever *owns* the evaluator — the service, or a test driving
    /// a hook directly. A hook only ever sees `&mut ProcessorSync`, so it
    /// cannot reach the evaluator through this.
    pub fn new(evaluator: &'a mut Evaluator) -> Self {
        Self { evaluator }
    }
}

impl ProcessorRegistry for ProcessorSync<'_> {
    fn register(&mut self, node: NodeId, processor: Arc<dyn crate::eval::NodeProcessor>) {
        self.evaluator.register(node, processor);
    }

    fn processor(&self, node: NodeId) -> Option<&Arc<dyn crate::eval::NodeProcessor>> {
        self.evaluator.processor(node)
    }

    fn invalidate_node(&mut self, node: NodeId) {
        self.evaluator.invalidate_node(node);
    }
}

/// Host-supplied policy run on the worker thread.
pub trait EvalWorkerHooks: Send + 'static {
    /// Refresh processor registrations according to `hint`. `document`
    /// carries the request's document snapshot so layer networks (recursively
    /// including subnets) can be registered alongside `graph`. The first
    /// request a worker sees is always escalated to
    /// [`InvalidationHint::Structural`], so implementations may treat
    /// `None` as a strict no-op.
    ///
    /// **A structural hint arrives with the evaluator already reset**
    /// ([`Evaluator::reset`]): registrations, caches, dirty flags and scope
    /// state are gone and the cache budget is intact. An implementation
    /// registers what the graph and document need and nothing more — it has
    /// no way to replace the evaluator, by design (see [`ProcessorSync`]).
    fn sync(
        &mut self,
        evaluator: &mut ProcessorSync<'_>,
        graph: &Graph,
        document: Option<&Document>,
        hint: &InvalidationHint,
    );

    /// Post-process a successful evaluation output (e.g. read a GPU frame
    /// back, or rasterize `Geometry` into a `FrameBuffer` for the viewer).
    /// Defaults to a pass-through.
    ///
    /// **`None` means the post-processing failed.** The worker then delivers
    /// `value` unchanged — the same picture a hook that swallowed its own
    /// error would have produced — but **does not cache it**. That
    /// distinction is the whole reason this returns an `Option`: the frame
    /// cache stores the finalized form and a hit never re-runs this method,
    /// so caching a fallback would freeze one transient failure (a readback
    /// that lost the device, a rasterize that ran out of memory) into a
    /// viewer that stays blank until the composition is edited.
    fn finalize(
        &mut self,
        value: &Arc<dyn NodeData>,
        ctx: &EvalContext,
    ) -> Option<Arc<dyn NodeData>> {
        let _ = ctx;
        Some(value.clone())
    }

    /// Settle a budget eviction list against caches this implementation owns
    /// — today the shared decode cache (`CACHE-8`), which lives beside the
    /// processors rather than inside the evaluator.
    ///
    /// Drop the values `evicted` names that you own, then return **every id
    /// that still needs an owner**: the ones you did not recognise, plus any
    /// your own caches were handed earlier and could not place. An empty
    /// argument therefore means "hand over what you are holding".
    ///
    /// The default owns nothing and hands the list straight back, which is
    /// what a hooks implementation without caches of its own must do — a
    /// list quietly dropped here leaves the budget counting fewer bytes than
    /// the process holds ([`crate::cache_budget`]).
    fn reconcile_evictions(&mut self, evicted: Vec<Evicted>) -> Vec<Evicted> {
        evicted
    }
}

/// Hand every id in the caches' eviction buffers to the cache that owns it.
///
/// One pass settles the list because each [`Evicted`] belongs to exactly one
/// of the caches sharing the budget — the node results, the output-stage
/// frames, and whatever the hooks own — and each of them sees the whole
/// list. What survives them all is an id nobody present owns, which is the
/// only kind that may be discarded ([`crate::cache_budget`]).
///
/// `frames` is optional because a render worker has no output-stage frame
/// cache: a render walks each frame once, so caching the finished picture
/// would only cost memory. **The routing itself is not optional**, which is
/// why both workers call this rather than each writing the sequence out —
/// two copies of budget-critical bookkeeping have to be kept identical by
/// hand, and nothing would enforce it.
pub(crate) fn settle_evictions<H: EvalWorkerHooks>(
    evaluator: &mut Evaluator,
    frames: Option<&SharedFrameCache>,
    hooks: &mut H,
) {
    let mut pending = evaluator.take_foreign_evictions();
    if let Some(frames) = frames {
        pending.extend(frames.take_foreign_evictions());
    }
    // Hooks first: the call both drops what it owns and surrenders what it
    // is holding, so the later legs see the decode cache's leftovers too.
    pending = hooks.reconcile_evictions(pending);
    if let Some(frames) = frames
        && !pending.is_empty()
    {
        frames.drop_evicted(&pending);
        pending = frames.take_foreign_evictions();
    }
    if !pending.is_empty() {
        evaluator.drop_evicted(&pending);
        pending = evaluator.take_foreign_evictions();
    }
    if !pending.is_empty() {
        tracing::debug!(
            count = pending.len(),
            "eviction ids no cache in this worker owns"
        );
    }
}

struct Request {
    inner: EvalRequest,
    generation: u64,
}

/// Read-ahead policy: how long the worker must be unoccupied before it fills
/// frames nobody has asked for, and how far ahead of the playhead it goes
/// (`CACHE-9`).
///
/// **Opt-in.** [`EvalService::spawn`] and
/// [`EvalService::spawn_with_budget`] leave it off, so every caller that
/// wants exactly the frames it requested — a render, a benchmark, a test
/// counting `process()` calls — keeps that. The application turns it on
/// through [`EvalServiceConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadAhead {
    /// How long both queues must stay empty before read-ahead starts.
    ///
    /// The threshold is what keeps speculation off the interactive path:
    /// evaluation is not interruptible, so a frame started too eagerly delays
    /// the next real request by its own cost. Waiting until nothing has
    /// arrived for this long means the user has stopped scrubbing.
    pub idle: Duration,
    /// How many frames past the playhead to fill.
    pub frames: u64,
}

impl ReadAhead {
    /// Idle threshold, 250 ms.
    ///
    /// Long enough that a scrub — which posts on every mouse move, tens of
    /// milliseconds apart — never trips it, short enough that letting go of
    /// the playhead starts filling before the user looks away.
    pub const DEFAULT_IDLE: Duration = Duration::from_millis(250);

    /// Frames filled ahead of the playhead, 24 — about a second of footage,
    /// which is the run a "play from here" gets through before the worker
    /// could have caught up on its own.
    pub const DEFAULT_FRAMES: u64 = 24;
}

impl Default for ReadAhead {
    fn default() -> Self {
        Self {
            idle: Self::DEFAULT_IDLE,
            frames: Self::DEFAULT_FRAMES,
        }
    }
}

/// Everything optional an [`EvalService`] can be spawned with.
#[derive(Clone, Default)]
pub struct EvalServiceConfig {
    /// The process cache budget the worker's caches report to.
    pub budget: Option<crate::cache_budget::SharedCacheBudget>,
    /// Read-ahead policy, or `None` to evaluate only what is requested.
    pub read_ahead: Option<ReadAhead>,
    /// Generation the first [`request`](EvalService::request) counts up from.
    ///
    /// Zero for a session's first worker. A worker that **replaces** another
    /// one — a GPU device epoch swap, where the old worker is shut down and a
    /// new one is built on the new device — starts from the old worker's
    /// [`latest_generation`](EvalService::latest_generation) instead. The
    /// consumer's fence is a generation, so a replacement that restarted at
    /// zero would have every frame of the new epoch rejected as stale until
    /// it caught up with the old numbering; carrying the number over is what
    /// keeps [`cancel_pending`](EvalService::cancel_pending)'s meaning across
    /// the boundary without a second token to pass around.
    pub generation: u64,
}

/// What the worker picked up.
enum Job {
    /// A request someone is waiting for.
    Interactive(Request),
    /// A frame read-ahead asked for. No generation, no `on_update`, and the
    /// first interactive request throws away whatever is still queued.
    Speculative(EvalRequest),
}

/// Outcome of a wait with nothing to do.
///
/// The job is boxed: a [`Job`] carries a whole [`EvalRequest`] — graph,
/// document, scoped targets — while the other two variants carry nothing, and
/// every `select!` arm would otherwise return that much stack.
enum Wait {
    Work(Box<Job>),
    /// Nothing arrived within the read-ahead threshold.
    Idle,
    /// The service was dropped.
    Closed,
}

/// Block until either queue has work, or — when `idle` is set — until that
/// long has passed with neither producing any.
///
/// The wait **is** the idle detector: "no interactive request for `idle`" is
/// read off the channel rather than off a clock the worker samples, so there
/// is no timer to drift and a test can set the threshold to zero and get a
/// deterministic "the queues are empty" trigger.
fn wait_for_work(
    rx: &Receiver<Request>,
    speculative: &Receiver<EvalRequest>,
    idle: Option<Duration>,
) -> Wait {
    let interactive = |result: Result<Request, crossbeam_channel::RecvError>| match result {
        Ok(request) => Wait::Work(Box::new(Job::Interactive(request))),
        Err(_) => Wait::Closed,
    };
    match idle {
        Some(idle) => select! {
            recv(rx) -> result => interactive(result),
            recv(speculative) -> result => match result {
                Ok(request) => Wait::Work(Box::new(Job::Speculative(request))),
                Err(_) => Wait::Closed,
            },
            default(idle) => Wait::Idle,
        },
        None => select! {
            recv(rx) -> result => interactive(result),
            recv(speculative) -> result => match result {
                Ok(request) => Wait::Work(Box::new(Job::Speculative(request))),
                Err(_) => Wait::Closed,
            },
        },
    }
}

/// Throw away every queued speculative request.
///
/// Called whenever an interactive request is picked up: read-ahead was
/// filling from a playhead position the user has since left, so the queue is
/// stale by construction (`cache-plan.md`: 対話要求が来たら投機は即破棄).
fn discard_speculative(speculative: &Receiver<EvalRequest>) {
    while speculative.try_recv().is_ok() {}
}

/// The interactive request read-ahead extends.
struct ReadAheadTemplate {
    graph: Graph,
    node: NodeId,
    comp: CompId,
    ctx: EvalContext,
    document: Arc<Document>,
}

impl ReadAheadTemplate {
    /// The template `request` provides, if it is one read-ahead can extend:
    /// a root-scope composition request with a document. Everything else — a
    /// render, a network preview, an inspection-only pull — has no playhead
    /// to run ahead of.
    fn of(request: &EvalRequest) -> Option<Self> {
        Some(Self {
            graph: request.graph.clone(),
            node: *request.nodes.first()?,
            comp: request.comp.filter(|_| request.path.is_empty())?,
            ctx: request.ctx,
            document: request.document.clone()?,
        })
    }

    /// Queue the frames after the playhead that are not cached already.
    ///
    /// The probe is [`SharedFrameCache::contains`], which records neither a
    /// hit nor a miss: speculation is not a request anyone made, and counting
    /// it would move the hit rate the logs and the tests read.
    fn queue(
        &self,
        speculative: &Sender<EvalRequest>,
        frames: &SharedFrameCache,
        config: ReadAhead,
    ) {
        let duration = self
            .document
            .compositions
            .get(&self.comp)
            .map_or(0, |comp| comp.duration_frames);
        let mut queued = 0u64;
        for offset in 1..=config.frames {
            let frame = self.ctx.frame.saturating_add(offset);
            if frame >= duration {
                break;
            }
            let ctx = self.ctx.with_frame(frame);
            if frames.contains(self.comp, &CacheIdentity::of_frame(&ctx)) {
                continue;
            }
            let request = EvalRequest {
                graph: self.graph.clone(),
                nodes: vec![self.node],
                scoped: Vec::new(),
                comp: Some(self.comp),
                path: Vec::new(),
                ctx,
                document: Some(self.document.clone()),
                hint: InvalidationHint::None,
            };
            if speculative.send(request).is_err() {
                return;
            }
            queued += 1;
        }
        if queued > 0 {
            tracing::debug!(
                from = self.ctx.frame,
                queued,
                "read-ahead queued speculative frames"
            );
        }
    }
}

/// The nodes a hint names, or `None` when it names none (`CACHE-7`).
fn params_of(hint: &InvalidationHint) -> Option<Vec<NodeId>> {
    match hint {
        InvalidationHint::Params(ids) => Some(ids.clone()),
        InvalidationHint::None | InvalidationHint::Structural => None,
    }
}

/// Union two coalesced requests' narrowing sets. **Absorbing**, not merging:
/// one request that named nothing means the document step holds a change no
/// node list explains, and the frame cache has to fall back to dropping whole
/// compositions.
fn merge_narrow(older: Option<Vec<NodeId>>, newer: Option<Vec<NodeId>>) -> Option<Vec<NodeId>> {
    let (mut older, newer) = (older?, newer?);
    for id in newer {
        if !older.contains(&id) {
            older.push(id);
        }
    }
    Some(older)
}

/// Handle owned by the UI thread. Dropping it shuts the worker down.
pub struct EvalService {
    tx: Option<Sender<Request>>,
    /// The read-ahead queue (`CACHE-9`). Separate from `tx` so an
    /// interactive request can discard everything on it.
    speculative: Option<Sender<EvalRequest>>,
    generation: u64,
    worker: Option<JoinHandle<()>>,
    frames: SharedFrameCache,
}

impl EvalService {
    /// Spawn the worker thread with an **unbounded** result cache.
    ///
    /// `on_update` is invoked on the worker thread for every completed
    /// evaluation; forward it to the UI through a channel or executor of the
    /// host's choosing. An application uses
    /// [`spawn_with_budget`](Self::spawn_with_budget) so the worker's cache
    /// is accounted for with every other cache in the process.
    pub fn spawn<H, F>(hooks: H, on_update: F) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(EvalUpdate) + Send + 'static,
    {
        Self::spawn_with_config(hooks, EvalServiceConfig::default(), on_update)
    }

    /// Spawn the worker thread with a result cache bounded by `budget`.
    ///
    /// The budget is created by the application and shared with every other
    /// cache — notably the texture pool, whose idle allowance is whatever the
    /// resident side of the same VRAM tier leaves over. The worker builds its
    /// [`Evaluator`] on its own thread, so the budget has to be handed in
    /// here rather than attached afterwards.
    pub fn spawn_with_budget<H, F>(
        hooks: H,
        budget: crate::cache_budget::SharedCacheBudget,
        on_update: F,
    ) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(EvalUpdate) + Send + 'static,
    {
        Self::spawn_with_config(
            hooks,
            EvalServiceConfig {
                budget: Some(budget),
                ..EvalServiceConfig::default()
            },
            on_update,
        )
    }

    /// Spawn the worker thread with everything optional spelled out — the
    /// constructor the application uses, because it is the one that can turn
    /// read-ahead on (`CACHE-9`).
    pub fn spawn_with_config<H, F>(mut hooks: H, config: EvalServiceConfig, on_update: F) -> Self
    where
        H: EvalWorkerHooks,
        F: Fn(EvalUpdate) + Send + 'static,
    {
        let EvalServiceConfig {
            budget,
            read_ahead,
            generation: initial_generation,
        } = config;
        let (tx, rx) = unbounded::<Request>();
        let (speculative_tx, speculative_rx) = unbounded::<EvalRequest>();
        let worker_speculative_tx = speculative_tx.clone();
        let frames = SharedFrameCache::new(budget.clone());
        let worker_frames = frames.clone();
        let worker = std::thread::Builder::new()
            .name("ravel-eval-service".into())
            .spawn(move || {
                let frames = worker_frames;
                // Kept beside the evaluator so the per-request log line can
                // say what the caches are spending *against* — a byte figure
                // with no ceiling next to it says nothing about pressure.
                let worker_budget = budget.clone();
                let mut evaluator = match budget {
                    Some(budget) => Evaluator::with_budget(budget),
                    None => Evaluator::new(),
                };
                // The document the frame cache last invalidated against. Kept
                // here rather than read back from the evaluator because a
                // structural resync clears the evaluator's copy, and the
                // frame cache must still be able to tell an edit from a
                // composition switch.
                // "An interactive request is waiting", as a predicate a
                // read-ahead evaluation can ask between nodes.
                let interrupt: crate::eval::CancelCheck = {
                    let rx = rx.clone();
                    Arc::new(move || !rx.is_empty())
                };
                let mut cached_document: Option<Arc<Document>> = None;
                let mut first = true;
                // What read-ahead would extend, and whether it already has
                // for this playhead position (`CACHE-9`).
                let mut template: Option<ReadAheadTemplate> = None;
                let mut filled = false;
                loop {
                    // Interactive work always wins, and picking it up drops
                    // every queued speculative frame: read-ahead was filling
                    // from a playhead the user has moved off.
                    let job = match rx.try_recv() {
                        Ok(request) => {
                            discard_speculative(&speculative_rx);
                            Job::Interactive(request)
                        }
                        Err(TryRecvError::Disconnected) => break,
                        Err(TryRecvError::Empty) => match speculative_rx.try_recv() {
                            Ok(request) => Job::Speculative(request),
                            Err(_) => {
                                // Nothing to do. The wait doubles as the idle
                                // detector, but only while there is something
                                // left to fill.
                                let idle = read_ahead
                                    .filter(|_| !filled && template.is_some())
                                    .map(|config| config.idle);
                                match wait_for_work(&rx, &speculative_rx, idle) {
                                    Wait::Work(job) => {
                                        if matches!(*job, Job::Interactive(_)) {
                                            discard_speculative(&speculative_rx);
                                        }
                                        *job
                                    }
                                    Wait::Idle => {
                                        if let (Some(config), Some(template)) =
                                            (read_ahead, &template)
                                        {
                                            template.queue(&worker_speculative_tx, &frames, config);
                                        }
                                        filled = true;
                                        continue;
                                    }
                                    Wait::Closed => break,
                                }
                            }
                        },
                    };
                    // Latest-wins: drain everything queued behind the first
                    // request, merging hints so skipped rebuilds still occur.
                    // A speculative job is never coalesced — read-ahead posts
                    // one request per frame and wants all of them.
                    let (mut request, generation, speculative, coalesced, mut narrow) = match job {
                        Job::Interactive(first_req) => {
                            filled = false;
                            let mut req = first_req;
                            let mut coalesced = 0u32;
                            // The nodes the frame cache may narrow this
                            // document step to (`CACHE-7`). Tracked beside
                            // the merged hint rather than read back off it:
                            // `InvalidationHint::merge` folds `None` into
                            // `Params` (documented as "nothing changed", but
                            // shell edits post it *with* a new document), so
                            // a merged `Params` cannot tell whether an
                            // unexplained edit rode along. Narrowing
                            // therefore requires that every coalesced request
                            // named its nodes.
                            let mut narrow = params_of(&req.inner.hint);
                            while let Ok(newer) = rx.try_recv() {
                                coalesced += 1;
                                let prev_hint = req.inner.hint;
                                req = newer;
                                narrow = merge_narrow(narrow, params_of(&req.inner.hint));
                                req.inner.hint = prev_hint.merge(std::mem::replace(
                                    &mut req.inner.hint,
                                    InvalidationHint::None,
                                ));
                            }
                            let generation = req.generation;
                            (req.inner, generation, false, coalesced, narrow)
                        }
                        Job::Speculative(request) => (request, 0, true, 0, None),
                    };
                    if first {
                        request.hint = InvalidationHint::Structural;
                        narrow = None;
                        first = false;
                    }
                    tracing::debug!(
                        generation,
                        speculative,
                        targets = request.nodes.len(),
                        frame = request.ctx.frame,
                        hint = ?request.hint,
                        path_depth = request.path.len(),
                        coalesced,
                        "eval request picked up"
                    );
                    // A structural resync starts from an empty evaluator, and
                    // the service performs that reset itself. Hooks used to
                    // do it by assignment, which also discarded the budget
                    // the service owns; keeping it here means the one place
                    // that knows about the budget is the one place that
                    // clears the evaluator.
                    if matches!(request.hint, InvalidationHint::Structural) {
                        evaluator.reset();
                    }
                    hooks.sync(
                        &mut ProcessorSync::new(&mut evaluator),
                        &request.graph,
                        request.document.as_deref(),
                        &request.hint,
                    );
                    // The document diff drives scoped cache invalidation
                    // (network edits, shell edits, layer.ref referrers).
                    // Installed strictly *after* the reset above, which drops
                    // any document installed beforehand.
                    if let Some(document) = &request.document {
                        evaluator.set_document(document.clone());
                        // The frame cache reads the same diff: many document
                        // commits carry `InvalidationHint::None` and rely on
                        // it, so a hint-driven frame cache would serve those
                        // edits a stale picture.
                        frames.sync_document(
                            cached_document.as_deref(),
                            document,
                            narrow.as_deref(),
                        );
                        cached_document = Some(document.clone());
                    }
                    // Only the first target is the composition output, and
                    // only a root-scope request with a document has the
                    // invalidation signal this layer needs.
                    let cached_comp = request
                        .comp
                        .filter(|_| request.path.is_empty() && request.document.is_some());
                    // Read-ahead runs at the speculative budget rank and
                    // gives way the moment an interactive request lands: the
                    // queue-level discard only covers frames that have not
                    // started, and one heavy frame is exactly the case that
                    // would otherwise delay a scrub. The predicate reads the
                    // channel, never a clock. Installed after the structural
                    // reset above, which would otherwise clear it.
                    evaluator.set_read_ahead(speculative.then(|| interrupt.clone()));
                    let frame_identity = CacheIdentity::of_frame(&request.ctx);
                    let started = std::time::Instant::now();
                    let mut results = Vec::with_capacity(request.nodes.len());
                    let mut timings = Vec::new();
                    for (index, &node) in request.nodes.iter().enumerate() {
                        let frame_comp = cached_comp.filter(|_| index == 0);
                        if let Some(comp) = frame_comp
                            && let Some(value) = frames.get(comp, &frame_identity)
                        {
                            // A hit skips `evaluate_at` *and* `finalize`, so
                            // nothing is processed and no GPU frame is read
                            // back for this target.
                            results.push((node, Ok(value)));
                            continue;
                        }
                        // `finalize` reporting failure keeps its picture but
                        // loses its place in the cache: see the trait method.
                        //
                        // A hook that *panics* instead is not handled, on
                        // purpose. The unwind ends this thread, the request
                        // channel's receiver goes with it, and every later
                        // `request` is silently dropped — the application has
                        // no evaluation left at all, so the budget
                        // over-counting a frame nobody can reach is not the
                        // failure anyone is looking at. Catching it would put
                        // an `UnwindSafe` bound on every hooks implementation
                        // to protect accounting in a process that has already
                        // lost its evaluator.
                        let mut finalized = true;
                        let result = evaluator
                            .evaluate_at(&request.path, &request.graph, node, &request.ctx)
                            .map(|value| match hooks.finalize(&value, &request.ctx) {
                                Some(value) => value,
                                None => {
                                    finalized = false;
                                    value
                                }
                            });
                        if let (Some(comp), Ok(value)) = (frame_comp.filter(|_| finalized), &result)
                        {
                            frames.insert(comp, frame_identity, value.clone(), speculative);
                        }
                        // The budget's tiers are shared, so any of the three
                        // caches can push another's entry out. Settling after
                        // both the evaluation and the frame insert routes
                        // every id to its owner — an eviction nobody acts on
                        // leaves the budget counting fewer bytes than the
                        // process holds.
                        settle_evictions(&mut evaluator, Some(&frames), &mut hooks);
                        // Drained per target: `evaluate_at` clears the
                        // evaluator's timing buffer on entry, so reading it
                        // only after the loop would report the last target
                        // alone and silently blank the load readout of the
                        // composition output whenever a second target is
                        // requested.
                        timings.append(&mut evaluator.take_timings());
                        // One failing target must not cost the others their
                        // result: the viewer keeps drawing while an
                        // inspection target is broken, and vice versa. A
                        // cancelled read-ahead is not a failure and is not
                        // logged as one.
                        // `is_cancelled` rather than a `matches!` on the
                        // outer variant: `comp.network`, `subnet` and
                        // `layer.ref` run their inner graph from inside
                        // `process()`, so a nested cancellation arrives
                        // wrapped in `ProcessFailed`.
                        if let Err(err) = &result
                            && !err.is_cancelled()
                        {
                            tracing::debug!(
                                generation = generation,
                                node = node.raw(),
                                frame = request.ctx.frame,
                                %err,
                                "eval target failed"
                            );
                        }
                        results.push((node, result));
                    }
                    // Scoped targets come last and through the same evaluator,
                    // which is the whole point: the shell pull above already
                    // ran every layer network it composites, so a node inside
                    // one is served from the node cache rather than pulled a
                    // second time.
                    //
                    // Neither the frame cache nor `finalize` applies here. Both
                    // exist for the composition output — the frame cache is
                    // keyed by `(comp, TimeKey)` alone, and `finalize` is the
                    // display transform. A geometry or a field put through
                    // either would be cached under the frame's key or handed
                    // back as display bytes.
                    let mut scoped = Vec::with_capacity(request.scoped.len());
                    for target in &request.scoped {
                        let result = evaluator.evaluate_at(
                            &target.path,
                            &target.graph,
                            target.node,
                            &target.ctx,
                        );
                        settle_evictions(&mut evaluator, Some(&frames), &mut hooks);
                        timings.append(&mut evaluator.take_timings());
                        if let Err(err) = &result
                            && !err.is_cancelled()
                        {
                            tracing::debug!(
                                generation = generation,
                                node = target.node.raw(),
                                frame = target.ctx.frame,
                                path_depth = target.path.len(),
                                %err,
                                "scoped eval target failed"
                            );
                        }
                        scoped.push(ScopedResult {
                            path: target.path.clone(),
                            node: target.node,
                            output: result,
                        });
                    }
                    let elapsed = started.elapsed();
                    // Per-request outcome: a frozen viewer with a stream of
                    // `ok = 0` results is an evaluation error; results that
                    // never reach the viewer were dropped by the consumer
                    // (stale generation); a result stream that stops entirely
                    // means no requests are being posted.
                    //
                    // The frame-cache figures ride along because the question
                    // they answer — "is the cache still working?" — is only
                    // readable as a ratio over a session, not from any one
                    // request (`CACHE-6`).
                    let frame_stats = frames.stats();
                    let node_stats = evaluator.cache_stats();
                    let budget_stats = worker_budget.as_ref().map(|budget| budget.stats());
                    tracing::debug!(
                        generation = generation,
                        frame = request.ctx.frame,
                        targets = results.len(),
                        ok = results.iter().filter(|(_, r)| r.is_ok()).count(),
                        scoped_targets = scoped.len(),
                        scoped_ok = scoped.iter().filter(|r| r.output.is_ok()).count(),
                        timings = timings.len(),
                        ?elapsed,
                        frames_cached = frame_stats.entries,
                        frame_hit_rate = frame_stats.hit_rate(),
                        frame_bytes_vram = frame_stats.bytes(crate::cache_budget::Tier::Vram),
                        frame_bytes_ram = frame_stats.bytes(crate::cache_budget::Tier::Ram),
                        node_hit_rate = node_stats.hit_rate(),
                        budget_ram_used =
                            budget_stats.map(|stats| stats.used(crate::cache_budget::Tier::Ram)),
                        budget_ram_limit =
                            budget_stats.map(|stats| stats.limit(crate::cache_budget::Tier::Ram)),
                        budget_vram_used =
                            budget_stats.map(|stats| stats.used(crate::cache_budget::Tier::Vram)),
                        budget_vram_limit =
                            budget_stats.map(|stats| stats.limit(crate::cache_budget::Tier::Vram)),
                        "eval result sent"
                    );
                    if speculative {
                        // Deliberately silent: read-ahead writes to the cache
                        // and nothing else. Emitting an update here would
                        // replace what the viewer is showing with a frame
                        // nobody asked for.
                        continue;
                    }
                    // The playhead read-ahead runs on from, refreshed after
                    // every interactive request.
                    template = read_ahead.and_then(|_| ReadAheadTemplate::of(&request));
                    on_update(EvalUpdate {
                        generation,
                        frame: request.ctx.frame,
                        results,
                        scoped,
                        timings,
                    });
                }
            })
            .expect("failed to spawn eval service worker");
        Self {
            tx: Some(tx),
            speculative: Some(speculative_tx),
            generation: initial_generation,
            worker: Some(worker),
            frames,
        }
    }

    /// The output-stage frame cache this service fills (`CACHE-5`).
    ///
    /// Shared with the worker, so the UI thread may read
    /// [`cached_ranges`](SharedFrameCache::cached_ranges) for the timeline's
    /// cache band and [`stats`](SharedFrameCache::stats) for diagnostics
    /// while an evaluation is running.
    pub fn frame_cache(&self) -> &SharedFrameCache {
        &self.frames
    }

    /// Post an evaluation request and return its generation number.
    pub fn request(&mut self, request: EvalRequest) -> u64 {
        self.generation += 1;
        let generation = self.generation;
        if let Some(tx) = &self.tx {
            let _ = tx.send(Request {
                inner: request,
                generation,
            });
        }
        generation
    }

    /// Post a read-ahead request (`CACHE-9`).
    ///
    /// Deliberately `&self` and returning nothing: speculation **does not
    /// advance the generation**, so a result the viewer is still waiting for
    /// cannot be outdated by a frame nobody asked for. It also never reaches
    /// `on_update` — it only fills the frame cache — and the worker throws
    /// away everything still queued here the moment a real request arrives.
    pub fn request_speculative(&self, request: EvalRequest) {
        if let Some(tx) = &self.speculative {
            let _ = tx.send(request);
        }
    }

    /// Invalidate all in-flight results without posting a new request
    /// (e.g. when the selection is cleared and the viewer is blanked).
    /// Returns the new latest generation.
    pub fn cancel_pending(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Generation of the most recent `request` / `cancel_pending` call.
    /// Consumers publish updates monotonically (newer than the last one
    /// they published); after `cancel_pending` they fence at the returned
    /// generation so in-flight results cannot overwrite the cancellation.
    pub fn latest_generation(&self) -> u64 {
        self.generation
    }

    /// Stop the worker and hand back its thread handle, so a caller that has
    /// to know the worker is *gone* can wait for it.
    ///
    /// The stop order is the same one [`Drop`] gives — closing the channels —
    /// because closing them **is** the order: the worker finishes the
    /// evaluation it is in, fails its next `recv`, and returns. There is no
    /// cancellation token, so the wait is at worst one evaluation long.
    ///
    /// What this adds over dropping is only the handle. Joining it is the
    /// caller's business, and it must not happen on the UI thread — the
    /// reason [`Drop`] deliberately does not join. The caller that needs it
    /// is a device epoch swap: the old worker owns the old `Evaluator`, hooks
    /// and texture pool, and until its thread has returned those are still
    /// charged to the shared cache budget. Building the replacement before
    /// then puts two GPU caches on one accounting authority.
    ///
    /// Returns `None` only if the worker handle was already taken.
    pub fn shutdown(mut self) -> Option<JoinHandle<()>> {
        drop(self.tx.take());
        drop(self.speculative.take());
        self.worker.take()
    }
}

impl Drop for EvalService {
    fn drop(&mut self) {
        // Closing the channel lets the worker finish its current evaluation
        // and exit on its own. Do NOT join here: the drop may happen on the
        // UI thread (panel teardown, layout rebuild) and a join would block
        // it for up to one full evaluation. A caller that has to *know* the
        // worker is gone takes the handle with
        // [`EvalService::shutdown`](EvalService::shutdown) and joins it
        // somewhere it is allowed to block.
        drop(self.tx.take());
        drop(self.speculative.take());
        drop(self.worker.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_budget::{CacheBudgetConfig, SharedCacheBudget};
    use crate::eval::NodeProcessor;
    use crate::graph::{Node, ParameterValue};
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};
    use crate::types::{FrameRate, Scalar};
    use crossbeam_channel::Receiver;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn ctx() -> EvalContext {
        EvalContext::new(0, FPS, (16, 16))
    }

    /// A hooks implementation that owns one budgeted thing, the way
    /// `GpuEvalHooks` owns the shared decode cache (`CACHE-8`).
    #[derive(Default)]
    struct CachingHooks {
        held: Vec<crate::cache_budget::Reservation>,
        foreign: Vec<Evicted>,
    }

    impl CachingHooks {
        fn reserve(&mut self, budget: &crate::cache_budget::SharedCacheBudget, bytes: u64) {
            let (reservation, evicted) =
                budget.reserve(crate::cache_budget::CacheKind::MediaFrame, bytes);
            self.held.push(reservation);
            self.drop_owned(&evicted);
        }

        fn drop_owned(&mut self, evicted: &[Evicted]) {
            for entry in evicted {
                match self.held.iter().position(|held| held.id() == entry.id) {
                    Some(index) => {
                        self.held.swap_remove(index);
                    }
                    None => self.foreign.push(*entry),
                }
            }
        }
    }

    impl EvalWorkerHooks for CachingHooks {
        fn sync(
            &mut self,
            _evaluator: &mut ProcessorSync<'_>,
            _graph: &Graph,
            _document: Option<&Document>,
            _hint: &InvalidationHint,
        ) {
        }

        fn reconcile_evictions(&mut self, evicted: Vec<Evicted>) -> Vec<Evicted> {
            self.drop_owned(&evicted);
            std::mem::take(&mut self.foreign)
        }
    }

    /// `CACHE-8`: a worker has three caches on one pot, and the third lives
    /// behind the hooks. The settling pass has to reach it in both
    /// directions — an id it owns must arrive, and an id it does not own must
    /// leave — or the decode cache is back to leaking whatever the frame
    /// cache and the evaluator push out.
    #[test]
    fn settling_reaches_the_cache_the_hooks_own() {
        use crate::cache_budget::{CacheBudgetConfig, CacheKind, SharedCacheBudget, Tier};

        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 0,
            ram_bytes: 100,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        });
        let mut evaluator = Evaluator::with_budget(budget.clone());
        let frames = SharedFrameCache::new(Some(budget.clone()));
        let mut hooks = CachingHooks::default();

        // The hooks' cache fills the pot, then the frame cache pushes it out.
        let frame = crate::types::FrameBuffer::new_zeroed(2, 2);
        hooks.reserve(&budget, 80);
        frames.insert(
            CompId::new(1),
            CacheIdentity::of_frame(&ctx()),
            Arc::new(frame),
            false,
        );
        assert_eq!(hooks.held.len(), 1, "nothing has settled yet");

        settle_evictions(&mut evaluator, Some(&frames), &mut hooks);
        assert!(
            hooks.held.is_empty(),
            "the frame cache's eviction never reached the hooks' cache"
        );

        // And the other way: the hooks' cache is told to give up an id that
        // belongs to the frame cache, and must hand it on rather than drop it.
        hooks.reserve(&budget, 90);
        assert!(
            !hooks.foreign.is_empty(),
            "the hooks dropped an id they do not own on the floor"
        );
        settle_evictions(&mut evaluator, Some(&frames), &mut hooks);
        assert_eq!(
            frames.stats().entries,
            0,
            "the frame the hooks' reservation evicted is still resident"
        );
        assert_eq!(
            budget.stats().used(Tier::Ram),
            90,
            "the pot holds the hooks' reservation and nothing else"
        );
        let _ = CacheKind::MediaFrame;
    }

    fn value_node(id: u64, value: f32) -> Node {
        Node::new(NodeId::new(id), "test.value")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(value))
    }

    /// `upstream → downstream`: the shape an inspection target has relative to
    /// the composition output, so evaluating the downstream node also
    /// evaluates the upstream one.
    fn chain_graph(upstream: NodeId, downstream: NodeId) -> Graph {
        Graph::new()
            .add_node(value_node(upstream.raw(), 1.0))
            .unwrap()
            .add_node(value_node(downstream.raw(), 2.0).with_input("in", &[DataTypeId::SCALAR]))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                upstream,
                OutputPortIndex(0),
                downstream,
                InputPortIndex(0),
            )
            .unwrap()
    }

    /// Emits the node's `value` parameter; optionally blocks on a gate
    /// channel first and records the processing thread's name.
    struct GatedValue {
        value: f32,
        gate: Option<Receiver<()>>,
        process_count: Arc<AtomicUsize>,
        thread_name: Arc<Mutex<Option<String>>>,
    }

    impl NodeProcessor for GatedValue {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            _scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            *self.thread_name.lock().unwrap() = std::thread::current().name().map(String::from);
            self.process_count.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.recv_timeout(Duration::from_secs(5))
                    .expect("gate closed");
            }
            Ok(Arc::new(Scalar(self.value)))
        }
    }

    /// Hooks that register a `GatedValue` for every graph node and log the
    /// hints they were synced with.
    struct StubHooks {
        gate: Option<Receiver<()>>,
        process_count: Arc<AtomicUsize>,
        thread_name: Arc<Mutex<Option<String>>>,
        hints: Arc<Mutex<Vec<InvalidationHint>>>,
    }

    impl StubHooks {
        fn register_node(&self, evaluator: &mut ProcessorSync<'_>, node: &Node) {
            let value = node
                .parameters
                .iter()
                .find(|p| p.key == "value")
                .and_then(|p| match p.value {
                    ParameterValue::Float(v) => Some(v),
                    _ => None,
                })
                .unwrap_or(0.0);
            evaluator.register(
                node.id,
                Arc::new(GatedValue {
                    value,
                    gate: self.gate.clone(),
                    process_count: self.process_count.clone(),
                    thread_name: self.thread_name.clone(),
                }),
            );
        }
    }

    impl EvalWorkerHooks for StubHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            self.hints.lock().unwrap().push(hint.clone());
            match hint {
                InvalidationHint::None => {}
                InvalidationHint::Params(ids) => {
                    for id in ids {
                        if let Some(node) = graph.node(*id) {
                            self.register_node(evaluator, node);
                        }
                    }
                }
                InvalidationHint::Structural => {
                    // Reset by the service before `sync`.
                    for node in graph.nodes() {
                        self.register_node(evaluator, node);
                    }
                }
            }
        }
    }

    fn req(graph: Graph, node: NodeId, hint: InvalidationHint) -> EvalRequest {
        req_multi(graph, vec![node], hint)
    }

    fn req_multi(graph: Graph, nodes: Vec<NodeId>, hint: InvalidationHint) -> EvalRequest {
        EvalRequest {
            graph,
            nodes,
            scoped: Vec::new(),
            comp: None,
            path: Vec::new(),
            ctx: ctx(),
            document: None,
            hint,
        }
    }

    /// The scalar of the `index`-th target of a multi-target update.
    fn scalar_at(update: &EvalUpdate, index: usize) -> f32 {
        update.results[index]
            .1
            .as_ref()
            .expect("evaluation succeeded")
            .downcast_ref::<Scalar>()
            .expect("scalar output")
            .0
    }

    fn scalar_of(update: &EvalUpdate) -> f32 {
        assert_eq!(update.results.len(), 1, "single-target update expected");
        scalar_at(update, 0)
    }

    #[test]
    fn latest_wins_coalesces_queued_requests() {
        let (gate_tx, gate_rx) = unbounded();
        let (update_tx, update_rx) = unbounded();
        let process_count = Arc::new(AtomicUsize::new(0));
        let hints = Arc::new(Mutex::new(Vec::new()));
        let hooks = StubHooks {
            gate: Some(gate_rx),
            process_count: process_count.clone(),
            thread_name: Arc::new(Mutex::new(None)),
            hints: hints.clone(),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let gen1 = service.request(req(
            Graph::new().add_node(value_node(1, 1.0)).unwrap(),
            node,
            InvalidationHint::None,
        ));
        // Wait until the worker is inside process() for gen1.
        while process_count.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        // Queue three more scrub ticks while the worker is busy.
        for (i, value) in [2.0f32, 3.0, 4.0].iter().enumerate() {
            let graph = Graph::new().add_node(value_node(1, *value)).unwrap();
            let generation =
                service.request(req(graph, node, InvalidationHint::Params(vec![node])));
            assert_eq!(generation, gen1 + i as u64 + 1);
        }
        // Release gen1, then the (single, coalesced) follow-up evaluation.
        gate_tx.send(()).unwrap();
        gate_tx.send(()).unwrap();

        let first = update_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("gen1 update");
        assert_eq!(first.generation, gen1);
        assert_eq!(scalar_of(&first), 1.0);

        let second = update_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("coalesced update");
        assert_eq!(second.generation, service.latest_generation());
        assert_eq!(scalar_of(&second), 4.0);

        // Generations 2 and 3 were skipped: exactly two evaluations ran.
        assert_eq!(process_count.load(Ordering::SeqCst), 2);
        assert!(update_rx.try_recv().is_err());
    }

    #[test]
    fn first_request_escalates_to_structural_and_params_rebuilds() {
        let (update_tx, update_rx) = unbounded();
        let hints = Arc::new(Mutex::new(Vec::new()));
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: hints.clone(),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph_v1 = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        service.request(req(graph_v1.clone(), node, InvalidationHint::None));
        let first = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_of(&first), 1.0);

        // Parameter edit: only the changed node is re-registered and the
        // new value takes effect.
        let graph_v2 = Graph::new().add_node(value_node(1, 2.0)).unwrap();
        service.request(req(graph_v2, node, InvalidationHint::Params(vec![node])));
        let second = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_of(&second), 2.0);

        let hints = hints.lock().unwrap();
        assert_eq!(hints[0], InvalidationHint::Structural, "first escalated");
        assert_eq!(hints[1], InvalidationHint::Params(vec![node]));
    }

    #[test]
    fn structural_swap_follows_new_graph() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph_a = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        service.request(req(graph_a, NodeId::new(1), InvalidationHint::None));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // Undo/redo-style swap: different node set entirely.
        let graph_b = Graph::new().add_node(value_node(2, 9.0)).unwrap();
        service.request(req(graph_b, NodeId::new(2), InvalidationHint::Structural));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.results[0].0, NodeId::new(2));
        assert_eq!(scalar_of(&update), 9.0);
    }

    // ---- multiple targets per request --------------------------------------

    /// A request names one or more outputs and the update carries one entry
    /// per target, positionally aligned with `EvalRequest::nodes`.
    #[test]
    fn every_requested_target_reports_its_own_result() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph = Graph::new()
            .add_node(value_node(1, 1.0))
            .unwrap()
            .add_node(value_node(2, 2.0))
            .unwrap();
        service.request(req_multi(
            graph,
            vec![NodeId::new(1), NodeId::new(2)],
            InvalidationHint::None,
        ));

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.results.len(), 2, "one result per requested target");
        assert_eq!(update.results[0].0, NodeId::new(1));
        assert_eq!(update.results[1].0, NodeId::new(2));
        assert_eq!(scalar_at(&update, 0), 1.0);
        assert_eq!(scalar_at(&update, 1), 2.0);
        // One update for the whole request, not one per target: the request
        // is the unit the latest-wins queue coalesces.
        assert!(update_rx.try_recv().is_err());
    }

    /// The reason the targets share a request rather than a second service:
    /// they share the evaluator's cache. A target upstream of another is
    /// already computed by the time its own turn comes, which shows up as the
    /// *absence* of a second timing entry for it (`take_timings` reports only
    /// freshly processed nodes).
    #[test]
    fn a_target_upstream_of_another_hits_the_shared_cache() {
        let (update_tx, update_rx) = unbounded();
        let process_count = Arc::new(AtomicUsize::new(0));
        let hooks = StubHooks {
            gate: None,
            process_count: process_count.clone(),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let upstream = NodeId::new(1);
        let downstream = NodeId::new(2);
        service.request(req_multi(
            chain_graph(upstream, downstream),
            vec![downstream, upstream],
            InvalidationHint::None,
        ));

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_at(&update, 0), 2.0);
        assert_eq!(scalar_at(&update, 1), 1.0);

        assert_eq!(
            update
                .timings
                .iter()
                .filter(|(id, _)| *id == upstream)
                .count(),
            1,
            "the second target re-processed the shared upstream: {:?}",
            update.timings
        );
        // Counting alone would also pass if the chain edge were lost: the
        // upstream would then be processed once too, just by the *second*
        // target instead of as the first target's dependency. Order is what
        // separates a real cache hit from that silent regression — the
        // upstream has to be processed while the first target is pulling it.
        assert_eq!(
            update.timings.len(),
            2,
            "expected one timing per distinct node: {:?}",
            update.timings
        );
        assert_eq!(
            update.timings[0].0, upstream,
            "the upstream was not processed as the first target's dependency: {:?}",
            update.timings
        );
        // The aggregate spans every target: reading the evaluator's timings
        // only after the last one would drop the first target's entirely,
        // because `evaluate_at` clears the buffer on entry.
        assert!(
            update.timings.iter().any(|(id, _)| *id == downstream),
            "the first target's own timing was lost: {:?}",
            update.timings
        );
        assert_eq!(
            process_count.load(Ordering::SeqCst),
            2,
            "two distinct nodes, so two process() calls"
        );
    }

    // ---- scoped targets ----------------------------------------------------

    fn scoped_scalar(update: &EvalUpdate, index: usize) -> f32 {
        update.scoped[index]
            .output
            .as_ref()
            .expect("scoped evaluation succeeded")
            .downcast_ref::<Scalar>()
            .expect("scalar output")
            .0
    }

    /// The reason scoped targets ride on the composition's request rather than
    /// on a service of their own: they run through the **same** [`Evaluator`],
    /// so a node the request's own pull already computed at that scope is a
    /// cache hit. A second evaluator would process it again.
    #[test]
    fn a_scoped_target_hits_the_cache_of_the_requests_own_pull() {
        let (update_tx, update_rx) = unbounded();
        let process_count = Arc::new(AtomicUsize::new(0));
        let hooks = StubHooks {
            gate: None,
            process_count: process_count.clone(),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph = Graph::new().add_node(value_node(1, 7.0)).unwrap();
        let path = vec![PathSegment::Layer(
            CompId::new(3),
            crate::id::LayerId::new(4),
        )];
        service.request(EvalRequest {
            graph: graph.clone(),
            nodes: vec![node],
            scoped: vec![ScopedTarget {
                path: path.clone(),
                graph,
                node,
                ctx: ctx(),
            }],
            comp: None,
            path: path.clone(),
            ctx: ctx(),
            document: None,
            hint: InvalidationHint::None,
        });

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.results.len(), 1, "`results` stays one per `nodes`");
        assert_eq!(scalar_of(&update), 7.0);
        assert_eq!(update.scoped.len(), 1, "one result per scoped target");
        assert_eq!(update.scoped[0].node, node);
        assert_eq!(update.scoped[0].path, path, "the scope travels back");
        assert_eq!(scoped_scalar(&update, 0), 7.0);
        assert_eq!(
            process_count.load(Ordering::SeqCst),
            1,
            "the scoped target re-processed a node the request had just pulled"
        );
        assert_eq!(
            update.timings.len(),
            1,
            "a cache hit contributes no timing: {:?}",
            update.timings
        );
    }

    /// The scope is not decoration: `evaluate_at` rebinds it per call, so the
    /// same node at another path is a different cache entry and is processed
    /// again. That is what keeps one layer network's result from standing in
    /// for another's when both hold the same `NodeId`.
    #[test]
    fn a_scoped_target_is_evaluated_under_the_scope_it_names() {
        let (update_tx, update_rx) = unbounded();
        let process_count = Arc::new(AtomicUsize::new(0));
        let hooks = StubHooks {
            gate: None,
            process_count: process_count.clone(),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph = Graph::new().add_node(value_node(1, 5.0)).unwrap();
        let other = vec![PathSegment::Layer(
            CompId::new(1),
            crate::id::LayerId::new(2),
        )];
        service.request(EvalRequest {
            graph: graph.clone(),
            nodes: vec![node],
            scoped: vec![ScopedTarget {
                path: other.clone(),
                graph,
                node,
                ctx: ctx(),
            }],
            comp: None,
            // The request's own pull runs at the root scope.
            path: Vec::new(),
            ctx: ctx(),
            document: None,
            hint: InvalidationHint::None,
        });

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.scoped[0].path, other);
        assert_eq!(scoped_scalar(&update, 0), 5.0);
        assert_eq!(
            process_count.load(Ordering::SeqCst),
            2,
            "the two scopes shared one cache entry, so the scope was ignored"
        );
    }

    /// A scoped target that cannot be evaluated reports its own `Err` and
    /// leaves the composition output — target 0 of `results` — alone.
    #[test]
    fn a_failing_scoped_target_leaves_the_composition_output_alone() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph = Graph::new().add_node(value_node(1, 4.0)).unwrap();
        service.request(EvalRequest {
            graph: graph.clone(),
            nodes: vec![NodeId::new(1)],
            scoped: vec![ScopedTarget {
                path: vec![PathSegment::Layer(
                    CompId::new(1),
                    crate::id::LayerId::new(1),
                )],
                graph,
                // Absent from the graph: nothing to pull.
                node: NodeId::new(99),
                ctx: ctx(),
            }],
            comp: None,
            path: Vec::new(),
            ctx: ctx(),
            document: None,
            hint: InvalidationHint::None,
        });

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(scalar_of(&update), 4.0);
        assert!(update.scoped[0].output.is_err());
    }

    /// One broken target must not blank the others: the viewer keeps drawing
    /// while an inspection target is unevaluable, and vice versa.
    #[test]
    fn a_failing_target_does_not_cost_the_others_their_result() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let missing = NodeId::new(99);
        let present = NodeId::new(1);
        service.request(req_multi(
            Graph::new().add_node(value_node(1, 1.0)).unwrap(),
            vec![missing, present],
            InvalidationHint::None,
        ));

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.results.len(), 2, "the failure keeps its slot");
        assert_eq!(update.results[0].0, missing);
        assert!(
            matches!(update.results[0].1, Err(EvalError::NodeNotFound(id)) if id == missing),
            "expected the first target to fail as missing"
        );
        assert_eq!(update.results[1].0, present);
        assert_eq!(scalar_at(&update, 1), 1.0);
    }

    #[test]
    fn evaluation_runs_on_the_worker_thread() {
        let (update_tx, update_rx) = unbounded();
        let thread_name = Arc::new(Mutex::new(None));
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: thread_name.clone(),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        service.request(req(graph, NodeId::new(1), InvalidationHint::None));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        assert_eq!(
            thread_name.lock().unwrap().as_deref(),
            Some("ravel-eval-service")
        );
    }

    #[test]
    fn cancel_pending_outdates_inflight_generations() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, move |update| {
            let _ = update_tx.send(update);
        });

        let graph = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        let generation = service.request(req(graph, NodeId::new(1), InvalidationHint::None));
        let cancelled_at = service.cancel_pending();
        assert!(cancelled_at > generation);

        // The update still arrives, but consumers comparing against
        // latest_generation() must treat it as stale.
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(update.generation < service.latest_generation());
    }

    #[test]
    fn drop_shuts_down_worker_without_hanging() {
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let service = EvalService::spawn(hooks, |_| {});
        drop(service);
    }

    /// `GPULOSS-2`: the epoch swap needs to know the old worker is *gone*,
    /// and the only stop order is still the channel close — so the handle
    /// must not come back joinable until the evaluation in flight has
    /// finished. The gate holds `process()` open, which is exactly the
    /// "recovery waits at worst one evaluation" case the plan accepts instead
    /// of a cancellation token.
    #[test]
    fn shutdown_hands_back_a_handle_that_waits_out_the_running_evaluation() {
        let (gate_tx, gate_rx) = unbounded();
        let process_count = Arc::new(AtomicUsize::new(0));
        let hooks = StubHooks {
            gate: Some(gate_rx),
            process_count: process_count.clone(),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn(hooks, |_| {});
        let graph = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        service.request(req(graph, NodeId::new(1), InvalidationHint::None));
        while process_count.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }

        let handle = service.shutdown().expect("the worker handle");
        assert!(
            !handle.is_finished(),
            "closing the channels abandoned the evaluation in flight"
        );

        // Only now can the evaluation complete — and the join must then
        // return rather than block on a worker that never noticed the close.
        let _ = gate_tx.send(());
        handle.join().expect("the worker thread panicked");
    }

    /// `GPULOSS-2`: what the join is *for*. The worker's frame cache and node
    /// cache are charged to the session's budget, and they are only handed
    /// back when its thread returns. A swap that built the replacement before
    /// then would put two GPU caches on one accounting authority — so the
    /// budget after the join must be the same authority, emptied, not a new
    /// one and not a reset.
    #[test]
    fn shutdown_returns_the_workers_caches_to_the_same_budget() {
        use crate::cache_budget::Tier;

        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 0,
            ram_bytes: 1 << 20,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        });
        let limit_before = budget.stats().limit(Tier::Ram);
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn_with_budget(
            FrameHooks::new(Arc::new(AtomicUsize::new(0))),
            budget.clone(),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        service.request(frame_request(
            frame_graph(node),
            node,
            0,
            frame_document(),
            InvalidationHint::Structural,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            budget.stats().used(Tier::Ram) > 0,
            "the worker's caches never reached the budget"
        );

        service
            .shutdown()
            .expect("the worker handle")
            .join()
            .expect("the worker thread panicked");

        let stats = budget.stats();
        assert_eq!(
            stats.used(Tier::Ram),
            0,
            "the old worker's caches are still charged after its thread returned"
        );
        assert_eq!(
            stats.limit(Tier::Ram),
            limit_before,
            "the budget was rebuilt or reset instead of being handed back to"
        );
    }

    /// `GPULOSS-2`: a replacement worker starts where the one it replaces
    /// stopped. The consumer fences on a generation, so a replacement that
    /// restarted at zero would have every frame of the new epoch discarded as
    /// stale — and the in-flight results of the old one, which are all at or
    /// below the carried-over number, must still be discarded.
    #[test]
    fn the_configured_generation_carries_across_a_replacement() {
        let (update_tx, update_rx) = unbounded();
        let hooks = StubHooks {
            gate: None,
            process_count: Arc::new(AtomicUsize::new(0)),
            thread_name: Arc::new(Mutex::new(None)),
            hints: Arc::new(Mutex::new(Vec::new())),
        };
        let mut service = EvalService::spawn_with_config(
            hooks,
            EvalServiceConfig {
                generation: 7,
                ..EvalServiceConfig::default()
            },
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        assert_eq!(
            service.latest_generation(),
            7,
            "the replacement did not inherit the generation it was given"
        );
        let graph = Graph::new().add_node(value_node(1, 1.0)).unwrap();
        let generation = service.request(req(graph, NodeId::new(1), InvalidationHint::None));
        assert_eq!(
            generation, 8,
            "the first request of the new epoch fell on the inherited fence"
        );
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            update.generation, 8,
            "the frame the new epoch publishes carries a stale generation"
        );
    }

    /// Emits 1.0 when the evaluator has a document, errors otherwise —
    /// mirrors the document dependency of the shell processors
    /// (`comp.network`, `layer.ref`).
    struct DocProbe;

    impl NodeProcessor for DocProbe {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            anyhow::ensure!(scope.document().is_some(), "no document set");
            Ok(Arc::new(Scalar(1.0)))
        }
    }

    /// Hooks that re-register everything on Structural (like `GpuEvalHooks`),
    /// relying on the service having reset the evaluator first.
    struct ResettingHooks;

    impl EvalWorkerHooks for ResettingHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            if matches!(hint, InvalidationHint::Structural) {
                for node in graph.nodes() {
                    evaluator.register(node.id, Arc::new(DocProbe));
                }
            }
        }
    }

    /// A structural sync replaces the evaluator; the request's document must
    /// survive it (regression: the document was installed before sync and
    /// silently dropped, failing every document-dependent evaluation right
    /// after a structural change).
    #[test]
    fn document_survives_a_structural_evaluator_reset() {
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(ResettingHooks, move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "probe").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        service.request(EvalRequest {
            comp: None,
            graph,
            nodes: vec![node],
            scoped: Vec::new(),
            path: Vec::new(),
            ctx: ctx(),
            document: Some(Arc::new(Document::default())),
            // First request escalates to Structural anyway.
            hint: InvalidationHint::Structural,
        });

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (_, result) = update.results.into_iter().next().expect("one result");
        assert!(
            result.is_ok(),
            "document-dependent evaluation must succeed after a structural reset: {:?}",
            result.err().map(|e| e.to_string())
        );
    }

    /// The cache budget must survive a structural resync.
    ///
    /// The worker escalates its *first* request to `Structural`, so a hook
    /// that rebuilt the evaluator would throw the budget away before a single
    /// frame was evaluated — the node cache would be unbounded in the real
    /// application while every unit test, which hands the budget straight to
    /// the cache, still passed (`cache-plan.md`: "a test that never reaches
    /// the limit passes even when the budget code is dead").
    #[test]
    fn the_cache_budget_survives_a_structural_resync() {
        let budget = SharedCacheBudget::new(CacheBudgetConfig::default());
        let (update_tx, update_rx) = unbounded();
        let mut service =
            EvalService::spawn_with_budget(ResettingHooks, budget.clone(), move |update| {
                let _ = update_tx.send(update);
            });

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "probe").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        service.request(EvalRequest {
            comp: None,
            graph,
            nodes: vec![node],
            scoped: Vec::new(),
            path: Vec::new(),
            ctx: ctx(),
            document: Some(Arc::new(Document::default())),
            hint: InvalidationHint::Structural,
        });

        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(update.results[0].1.is_ok(), "evaluation failed");
        assert!(
            budget.stats().entries > 0,
            "the evaluated value was cached outside the budget: the \
             structural resync dropped it"
        );
    }

    // ---- the output-stage frame cache (`CACHE-5`) --------------------------

    /// Emits a frame and counts how often it was asked to.
    struct FrameSource(Arc<AtomicUsize>);

    impl NodeProcessor for FrameSource {
        /// A composition output moves with the playhead; without this the
        /// evaluator's own entry is `TIMELESS` and answers every frame, and
        /// the frame cache would never be reached.
        fn is_time_dependent(&self) -> bool {
            true
        }

        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            _scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(crate::types::FrameBuffer::from_f32(
                2,
                2,
                vec![0.25; 2 * 2 * 4],
            )))
        }
    }

    /// Registers [`FrameSource`] for every node and counts `finalize` calls.
    ///
    /// `finalize` is where `GpuEvalHooks` reads a GPU frame back for the
    /// viewer — the only GPU→CPU transfer in the chain — so counting it is
    /// the readback counter `GPUCOMP-7` established, available headlessly.
    struct FrameHooks {
        processed: Arc<AtomicUsize>,
        finalized: Arc<AtomicUsize>,
        /// How many leading `finalize` calls report failure (`0`: none).
        fails_until: usize,
        /// Reports the frame of every evaluation that completed.
        ///
        /// Read-ahead produces no `EvalUpdate` to wait on, so without this a
        /// test would have to poll the shared cache — the fragility class
        /// that has already cost this repository two CI runs. A completed
        /// frame announces itself instead.
        done: Option<Sender<u64>>,
    }

    impl FrameHooks {
        fn new(processed: Arc<AtomicUsize>) -> Self {
            Self {
                processed,
                finalized: Arc::new(AtomicUsize::new(0)),
                fails_until: 0,
                done: None,
            }
        }

        fn counting_finalize(mut self, finalized: Arc<AtomicUsize>) -> Self {
            self.finalized = finalized;
            self
        }

        fn failing_until(mut self, fails_until: usize) -> Self {
            self.fails_until = fails_until;
            self
        }

        /// Announce every completed frame on `done`.
        fn reporting(mut self) -> (Self, Receiver<u64>) {
            let (tx, rx) = unbounded();
            self.done = Some(tx);
            (self, rx)
        }
    }

    impl EvalWorkerHooks for FrameHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            if matches!(hint, InvalidationHint::Structural) {
                for node in graph.nodes() {
                    evaluator.register(node.id, Arc::new(FrameSource(self.processed.clone())));
                }
            }
        }

        fn finalize(
            &mut self,
            value: &Arc<dyn NodeData>,
            ctx: &EvalContext,
        ) -> Option<Arc<dyn NodeData>> {
            self.finalized.fetch_add(1, Ordering::SeqCst);
            // `fails_until` finalize failures first, then success — the shape
            // of a transient readback loss.
            let ok = self.finalized.load(Ordering::SeqCst) > self.fails_until;
            if ok && let Some(done) = &self.done {
                let _ = done.send(ctx.frame);
            }
            ok.then(|| value.clone())
        }
    }

    fn comp_id() -> crate::id::CompId {
        crate::id::CompId::new(1)
    }

    fn frame_document() -> Arc<Document> {
        let mut document = Document::default();
        document.compositions.insert(
            comp_id(),
            Arc::new(crate::composition::Composition::new(
                comp_id(),
                "c",
                (2, 2),
                FPS,
                100,
            )),
        );
        Arc::new(document)
    }

    /// A composition whose `Arc` differs from `frame_document`'s — what a
    /// document edit looks like to the frame cache.
    fn edited_document() -> Arc<Document> {
        let mut document = Document::default();
        let mut comp = crate::composition::Composition::new(comp_id(), "c", (2, 2), FPS, 100);
        comp.name = "edited".into();
        document.compositions.insert(comp_id(), Arc::new(comp));
        Arc::new(document)
    }

    fn frame_request(
        graph: Graph,
        node: NodeId,
        frame: u64,
        document: Arc<Document>,
        hint: InvalidationHint,
    ) -> EvalRequest {
        EvalRequest {
            graph,
            nodes: vec![node],
            scoped: Vec::new(),
            comp: Some(comp_id()),
            path: Vec::new(),
            ctx: EvalContext::new(frame, FPS, (2, 2)),
            document: Some(document),
            hint,
        }
    }

    /// The core of `cache-plan.md`: scrubbing forward and back must not
    /// recompute anything. `take_timings` reports only nodes that actually
    /// ran, so an empty list *is* "no `process()` call".
    #[test]
    fn scrubbing_back_over_a_visited_frame_processes_nothing() {
        let processed = Arc::new(AtomicUsize::new(0));
        let finalized = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks::new(processed.clone()).counting_finalize(finalized.clone()),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();

        for frame in [0u64, 1] {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert_eq!(processed.load(Ordering::SeqCst), 2, "two distinct frames");
        let readbacks = finalized.load(Ordering::SeqCst);

        // Back to a frame already visited.
        service.request(frame_request(
            graph,
            node,
            0,
            document,
            InvalidationHint::None,
        ));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        assert!(
            update.timings.is_empty(),
            "the revisited frame was recomputed: {:?}",
            update.timings
        );
        assert_eq!(
            processed.load(Ordering::SeqCst),
            2,
            "process() ran for a frame that was already cached"
        );
        // A hit skips `finalize` too, which is where a GPU-resident frame
        // would be read back to host memory.
        assert_eq!(
            finalized.load(Ordering::SeqCst),
            readbacks,
            "a cache hit paid for a GPU→CPU transfer"
        );
        assert!(update.results[0].1.is_ok(), "the hit produced no value");
        assert_eq!(service.frame_cache().stats().hits, 1);
    }

    /// `INSP-2`: a viewer display option changes the *bytes*, not the
    /// composite, and the switch has to cost exactly one re-finalize.
    ///
    /// This is the mechanism a display-channel switch uses: drop the output
    /// stage's frames (they hold finished display bytes, so a hit would serve
    /// the previous mode's picture) and request again with
    /// [`InvalidationHint::None`]. What the request must **not** do is mark
    /// anything dirty — the node results are still valid, and this is the
    /// test that says so: `process()` never runs a second time while
    /// `finalize` does.
    #[test]
    fn invalidating_the_finished_frames_refinalizes_without_reprocessing() {
        let processed = Arc::new(AtomicUsize::new(0));
        let finalized = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks::new(processed.clone()).counting_finalize(finalized.clone()),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();

        service.request(frame_request(
            graph.clone(),
            node,
            0,
            document.clone(),
            InvalidationHint::Structural,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(processed.load(Ordering::SeqCst), 1);
        assert_eq!(finalized.load(Ordering::SeqCst), 1);

        // What the host does when the user picks another display channel.
        service.frame_cache().invalidate_comp(comp_id());
        service.request(frame_request(
            graph,
            node,
            0,
            document,
            InvalidationHint::None,
        ));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        assert!(update.results[0].1.is_ok(), "the frame came back empty");
        assert_eq!(
            finalized.load(Ordering::SeqCst),
            2,
            "the transform did not run again, so the viewer would keep the \
             bytes of the previous mode"
        );
        assert_eq!(
            processed.load(Ordering::SeqCst),
            1,
            "the node was evaluated again: a display option reached the \
             evaluator's cache identity or marked something dirty"
        );
        assert!(
            update.timings.is_empty(),
            "the composite was recomputed: {:?}",
            update.timings
        );
    }

    /// A `finalize` failure must not become a permanent one.
    ///
    /// The cache stores the *finalized* form and a hit never re-runs
    /// `finalize`, so caching the fallback a failed readback returns would
    /// freeze one transient loss into a viewer that never recovers. The
    /// failure is delivered, not stored, and the next request retries.
    #[test]
    fn a_failed_finalize_is_not_cached_and_is_retried() {
        let finalized = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks::new(Arc::new(AtomicUsize::new(0)))
                .counting_finalize(finalized.clone())
                .failing_until(1),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();

        service.request(frame_request(
            graph.clone(),
            node,
            0,
            document.clone(),
            InvalidationHint::None,
        ));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            update.results[0].1.is_ok(),
            "the fallback picture was not delivered"
        );
        assert_eq!(
            service.frame_cache().stats().entries,
            0,
            "the failed finalize was cached"
        );

        // Same frame again: the retry reaches `finalize`, succeeds, and only
        // now is the frame worth keeping.
        service.request(frame_request(
            graph,
            node,
            0,
            document,
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            finalized.load(Ordering::SeqCst),
            2,
            "the retry never reached finalize"
        );
        assert_eq!(service.frame_cache().stats().entries, 1);
    }

    /// The frame cache follows the document, not the invalidation hint: many
    /// document commits carry `InvalidationHint::None` and rely on the
    /// evaluator's own diff, so a hint-driven frame cache would serve those
    /// edits a stale picture.
    #[test]
    fn a_document_edit_drops_the_frames_even_without_a_hint() {
        let processed = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(FrameHooks::new(processed.clone()), move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();

        service.request(frame_request(
            graph.clone(),
            node,
            0,
            frame_document(),
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(service.frame_cache().stats().entries, 1);

        // Same frame, edited composition, weakest possible hint.
        service.request(frame_request(
            graph,
            node,
            0,
            edited_document(),
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            service.frame_cache().stats().hits,
            0,
            "the edit was served the pre-edit frame"
        );
    }

    /// The band's recompute guard (`CACHE-6`): an evaluation served from the
    /// frame cache changes nothing, so the UI thread must be able to see that
    /// without walking every entry. Scrubbing back over visited frames is
    /// exactly when a user generates the most evaluations.
    #[test]
    fn a_cache_hit_leaves_the_frame_cache_version_alone() {
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks::new(Arc::new(AtomicUsize::new(0))),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();
        let frames = service.frame_cache().clone();
        let post = |service: &mut EvalService, frame: u64| {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        };

        post(&mut service, 0);
        post(&mut service, 1);
        let after_fill = frames.version();
        // Back over a frame already cached: a hit, nothing stored.
        post(&mut service, 0);
        assert_eq!(
            frames.version(),
            after_fill,
            "a cache hit moved the version and would force a band recompute"
        );
        // A new frame does move it.
        post(&mut service, 2);
        assert_ne!(frames.version(), after_fill);
    }

    /// A request that names no composition keeps today's behaviour exactly:
    /// a render and a benchmark evaluate rather than share the interactive
    /// cache.
    #[test]
    fn a_request_without_a_composition_is_not_frame_cached() {
        let processed = Arc::new(AtomicUsize::new(0));
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(FrameHooks::new(processed.clone()), move |update| {
            let _ = update_tx.send(update);
        });

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        for _ in 0..2 {
            let mut request = frame_request(
                graph.clone(),
                node,
                0,
                frame_document(),
                InvalidationHint::None,
            );
            request.comp = None;
            service.request(request);
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        // The evaluator's own single-entry cache still answers the repeat,
        // so what this pins is the *frame* cache staying out of it.
        assert!(processed.load(Ordering::SeqCst) >= 1);
        assert_eq!(service.frame_cache().stats().entries, 0);
        assert_eq!(service.frame_cache().stats().requests(), 0);
    }

    /// The band the Timeline draws (`CACHE-6`) is `cached_ranges` over the
    /// frames playback has already produced: it grows as the playhead
    /// advances and is gone the moment the composition is edited.
    #[test]
    fn the_cached_range_grows_with_playback_and_vanishes_on_an_edit() {
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn(
            FrameHooks::new(Arc::new(AtomicUsize::new(0))),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();
        let frames = service.frame_cache().clone();
        let ranges = || frames.cached_ranges(comp_id(), &EvalContext::new(0, FPS, (2, 2)));

        for frame in 0..3u64 {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                ranges(),
                vec![0..frame + 1],
                "the band did not follow the playhead"
            );
        }

        // An edit to the composition, with the weakest hint there is.
        service.request(frame_request(
            graph,
            node,
            0,
            edited_document(),
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            ranges(),
            vec![0..1],
            "the edit left the pre-edit frames in the band"
        );
    }

    // ---- read-ahead (`CACHE-9`) --------------------------------------------

    /// A service whose read-ahead starts the moment both queues are empty.
    ///
    /// `idle: ZERO` is what keeps these tests deterministic — the trigger is
    /// "the worker has nothing to do", an event, rather than a wall-clock
    /// threshold a loaded machine can miss.
    fn spawn_reading_ahead<H: EvalWorkerHooks>(
        hooks: H,
        frames: u64,
    ) -> (EvalService, Receiver<EvalUpdate>) {
        let (tx, rx) = unbounded();
        let service = EvalService::spawn_with_config(
            hooks,
            EvalServiceConfig {
                budget: None,
                read_ahead: Some(ReadAhead {
                    idle: Duration::ZERO,
                    frames,
                }),
                generation: 0,
            },
            move |update| {
                let _ = tx.send(update);
            },
        );
        (service, rx)
    }

    fn frame_graph(node: NodeId) -> Graph {
        Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap()
    }

    /// The frames `done` reports, in the order they completed.
    fn completed(done: &Receiver<u64>, count: usize) -> Vec<u64> {
        (0..count)
            .map(|index| {
                done.recv_timeout(Duration::from_secs(5))
                    .unwrap_or_else(|_| panic!("only {index} of {count} frames completed"))
            })
            .collect()
    }

    /// The band grows past the playhead while nobody is asking for anything,
    /// and — the half a ranges-only assertion would miss — the frames it
    /// claims are frames a real request is actually served.
    #[test]
    fn read_ahead_extends_the_band_with_frames_a_request_would_hit() {
        let (hooks, done) = FrameHooks::new(Arc::new(AtomicUsize::new(0))).reporting();
        let (mut service, update_rx) = spawn_reading_ahead(hooks, 3);

        let node = NodeId::new(1);
        let frames = service.frame_cache().clone();
        // One document `Arc` throughout: a fresh one per request is a fresh
        // composition `Arc` too, and the diff would drop the cache.
        let document = frame_document();
        service.request(frame_request(
            frame_graph(node),
            node,
            0,
            document.clone(),
            InvalidationHint::None,
        ));
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // The interactive frame plus the three read-ahead filled, announced
        // by the hooks rather than discovered by polling the cache.
        let mut filled = completed(&done, 4);
        filled.sort_unstable();
        assert_eq!(filled, vec![0, 1, 2, 3]);
        assert_eq!(
            frames.cached_ranges(comp_id(), &EvalContext::new(0, FPS, (2, 2))),
            vec![0..4]
        );

        // The band is only honest if the identity read-ahead produced matches
        // the one the playhead will ask with. Requesting frame 3 the ordinary
        // way must be a hit, not a recompute. `take_timings` lists only nodes
        // that actually ran, so an empty list *is* "nothing was processed".
        service.request(frame_request(
            frame_graph(node),
            node,
            3,
            document,
            InvalidationHint::None,
        ));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            update.timings.is_empty(),
            "a read-ahead frame was recomputed when the playhead reached it: {:?}",
            update.timings
        );
        assert_eq!(service.frame_cache().stats().hits, 1);
    }

    /// Read-ahead fills the cache and nothing else: no update reaches the
    /// consumer, so the viewer keeps showing the frame the user is on.
    #[test]
    fn read_ahead_never_emits_an_update_or_moves_the_generation() {
        let (hooks, done) = FrameHooks::new(Arc::new(AtomicUsize::new(0))).reporting();
        let (mut service, update_rx) = spawn_reading_ahead(hooks, 3);

        let node = NodeId::new(1);
        let frames = service.frame_cache().clone();
        let generation = service.request(frame_request(
            frame_graph(node),
            node,
            0,
            frame_document(),
            InvalidationHint::None,
        ));
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.generation, generation);

        assert_eq!(completed(&done, 4).len(), 4);
        assert_eq!(frames.stats().entries, 4);
        assert!(
            update_rx.try_recv().is_err(),
            "a speculative frame was delivered to the consumer"
        );
        assert_eq!(
            service.latest_generation(),
            generation,
            "speculation advanced the latest-wins generation"
        );
    }

    /// An interactive request throws away everything read-ahead still has
    /// queued: the playhead has moved and those frames are filling from where
    /// it used to be.
    ///
    /// Gated rather than timed — the worker is held inside `process()` while
    /// the test queues the interactive request, so the discard happens on a
    /// sequence the test controls, not on a race it hopes to win.
    #[test]
    fn an_interactive_request_discards_the_queued_speculation() {
        let (gate_tx, gate_rx) = unbounded();
        let (start_tx, started) = unbounded();
        let (mut service, update_rx) = spawn_reading_ahead(GatedFrames::new(gate_rx, start_tx), 8);

        let node = NodeId::new(1);
        let document = frame_document();
        // Frame 0, released immediately.
        service.request(frame_request(
            frame_graph(node),
            node,
            0,
            document.clone(),
            InvalidationHint::None,
        ));
        gate_tx.send(()).unwrap();
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        // The worker now has eight speculative frames queued and is blocked
        // inside the first one — announced, not guessed.
        assert_eq!(
            started.recv_timeout(Duration::from_secs(5)).unwrap(),
            (0, node)
        );
        assert_eq!(
            started.recv_timeout(Duration::from_secs(5)).unwrap(),
            (1, node)
        );
        // The user scrubs. The request is queued *before* the gate is
        // released, so the worker cannot miss it.
        service.request(frame_request(
            frame_graph(node),
            node,
            50,
            document,
            InvalidationHint::None,
        ));
        gate_tx.send(()).unwrap(); // finishes the speculative frame
        gate_tx.send(()).unwrap(); // frame 50
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.frame, 50);

        // Frames 0 and 1 were finished before the scrub; 50 is the new
        // position. Everything read-ahead had queued between them was
        // dropped, and the read-ahead the *new* position starts is held at
        // its first frame by the (now empty) gate, so this set is stable.
        assert_eq!(
            service
                .frame_cache()
                .cached_ranges(comp_id(), &EvalContext::new(0, FPS, (2, 2))),
            vec![0..2, 50..51],
            "the queued speculation kept running after the playhead moved"
        );
    }

    /// A speculative frame **already being evaluated** gives way too, not
    /// just the ones still queued (`CACHE-9`). Dropping the queue alone would
    /// still let one expensive frame delay a scrub by its whole cost.
    ///
    /// The chain is two nodes so the yield point — immediately before a
    /// `process()` — falls *between* them: the upstream is held in the gate
    /// while the interactive request is posted, and the downstream must never
    /// run. The token accounting is what makes it deterministic: five
    /// `process()` calls are paid for, and a speculation that did not yield
    /// would spend one of frame 50's on the abandoned frame and never
    /// deliver its update.
    #[test]
    fn an_in_flight_speculative_frame_gives_way_to_an_interactive_request() {
        let (gate_tx, gate_rx) = unbounded();
        let (start_tx, started) = unbounded();
        let (mut service, update_rx) = spawn_reading_ahead(GatedFrames::new(gate_rx, start_tx), 8);

        let upstream = NodeId::new(1);
        let downstream = NodeId::new(2);
        let graph = || {
            Graph::new()
                .add_node(Node::new(upstream, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
                .unwrap()
                .add_node(
                    Node::new(downstream, "frame")
                        .with_output("out", DataTypeId::FRAME_BUFFER)
                        .with_input("in", &[DataTypeId::FRAME_BUFFER]),
                )
                .unwrap()
                .add_edge(
                    EdgeId::new(1),
                    upstream,
                    OutputPortIndex(0),
                    downstream,
                    InputPortIndex(0),
                )
                .unwrap()
        };
        let document = frame_document();

        service.request(frame_request(
            graph(),
            downstream,
            0,
            document.clone(),
            InvalidationHint::None,
        ));
        gate_tx.send(()).unwrap();
        gate_tx.send(()).unwrap();
        update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(completed_pair(&started), (0, upstream));
        assert_eq!(completed_pair(&started), (0, downstream));

        // Read-ahead has started frame 1 and is inside the upstream node.
        assert_eq!(completed_pair(&started), (1, upstream));
        service.request(frame_request(
            graph(),
            downstream,
            50,
            document,
            InvalidationHint::None,
        ));
        // One token finishes the upstream; the downstream of frame 1 must
        // then find the interactive request waiting and yield, leaving both
        // remaining tokens to frame 50.
        gate_tx.send(()).unwrap();
        gate_tx.send(()).unwrap();
        gate_tx.send(()).unwrap();
        let update = update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(update.frame, 50);

        // The abandoned frame reached exactly one `process()` — the yield
        // sits before the second — and produced no cache entry.
        assert_eq!(completed_pair(&started), (50, upstream));
        assert_eq!(completed_pair(&started), (50, downstream));
        assert_eq!(
            service
                .frame_cache()
                .cached_ranges(comp_id(), &EvalContext::new(0, FPS, (2, 2))),
            vec![0..1, 50..51],
            "the abandoned speculative frame was cached anyway"
        );
    }

    fn completed_pair(started: &Receiver<(u64, NodeId)>) -> (u64, NodeId) {
        started
            .recv_timeout(Duration::from_secs(5))
            .expect("a process() call was expected")
    }

    /// Emits a frame after waiting on a gate, announcing each `process()` it
    /// enters, so a test can hold the worker inside an evaluation and know
    /// exactly where it is.
    struct GatedFrames {
        gate: Receiver<()>,
        started: Sender<(u64, NodeId)>,
    }

    impl GatedFrames {
        fn new(gate: Receiver<()>, started: Sender<(u64, NodeId)>) -> Self {
            Self { gate, started }
        }
    }

    impl NodeProcessor for GatedFrames {
        fn is_time_dependent(&self) -> bool {
            true
        }

        fn process(
            &self,
            node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            _scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            let _ = self.started.send((ctx.frame, node.id));
            // A closed gate (the test finished) releases rather than panics:
            // the worker may still be parked in a speculative frame when the
            // service is dropped.
            let _ = self.gate.recv_timeout(Duration::from_secs(5));
            Ok(Arc::new(crate::types::FrameBuffer::from_f32(
                2,
                2,
                vec![0.25; 2 * 2 * 4],
            )))
        }
    }

    impl EvalWorkerHooks for GatedFrames {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            if matches!(hint, InvalidationHint::Structural) {
                for node in graph.nodes() {
                    evaluator.register(
                        node.id,
                        Arc::new(GatedFrames::new(self.gate.clone(), self.started.clone())),
                    );
                }
            }
        }
    }

    // ---- scoped invalidation (`CACHE-7`) -----------------------------------

    /// A composition output that mirrors the shell's real time gate: a layer
    /// contributes its network's value only at composition frames inside
    /// `[start_frame, start_frame + duration)`, exactly as
    /// `ravel_nodes::comp` gates the layer network. That gate is the property
    /// narrowing rests on, so the stand-in has to have it.
    struct GatedLayer;

    impl GatedLayer {
        /// The `value` parameter of the layer's single network node.
        fn value(layer: &crate::composition::Layer) -> f32 {
            layer
                .network
                .nodes()
                .find_map(|node| {
                    node.parameters
                        .iter()
                        .find(|param| param.key == "value")
                        .and_then(|param| match param.value {
                            ParameterValue::Float(v) => Some(v),
                            _ => None,
                        })
                })
                .unwrap_or(0.0)
        }
    }

    impl NodeProcessor for GatedLayer {
        fn is_time_dependent(&self) -> bool {
            true
        }

        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &crate::eval::ResolvedParams,
            scope: &mut dyn crate::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            let document = scope
                .document()
                .ok_or_else(|| anyhow::anyhow!("no document"))?;
            let comp = document
                .compositions
                .get(&comp_id())
                .ok_or_else(|| anyhow::anyhow!("no composition"))?;
            let frame = ctx.frame as i64;
            let value: f32 = comp
                .layers
                .iter()
                .filter(|layer| frame >= layer.start_frame && frame < layer.end_frame())
                .map(Self::value)
                .sum();
            Ok(Arc::new(crate::types::FrameBuffer::from_f32(
                1,
                1,
                vec![value; 4],
            )))
        }
    }

    struct GatedHooks;

    impl EvalWorkerHooks for GatedHooks {
        fn sync(
            &mut self,
            evaluator: &mut ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            hint: &InvalidationHint,
        ) {
            // The processor reads everything from the document handed to
            // `process`, so a parameter edit needs no re-registration — the
            // same shape `rebuild_on_node_change() == false` gives the real
            // GPU processors.
            if matches!(hint, InvalidationHint::Structural) {
                for node in graph.nodes() {
                    evaluator.register(node.id, Arc::new(GatedLayer));
                }
            }
        }
    }

    /// A document whose composition holds one layer at `[2, 5)` carrying
    /// `value`.
    fn layered_document(value: f32) -> Arc<Document> {
        let network = Graph::new().add_node(value_node(100, value)).unwrap();
        let layer = crate::composition::Layer::new(crate::id::LayerId::new(1), "l", network)
            .with_time(2, 0, 3);
        let mut comp = crate::composition::Composition::new(comp_id(), "c", (2, 2), FPS, 100);
        comp.layers.push_back(layer);
        let mut document = Document::default();
        document.compositions.insert(comp_id(), Arc::new(comp));
        Arc::new(document)
    }

    /// The criterion that keeps the optimisation honest: narrowing may only
    /// keep frames whose **picture** is unchanged.
    ///
    /// A set-theoretic check ("the drop covers the layer's span") would be
    /// tautological — it restates the implementation. So this evaluates the
    /// same edit twice, once through the narrowed frame cache and once
    /// through a service that has no frame cache at all, and compares the
    /// pictures frame by frame. A frame the narrowing kept that the layer
    /// edit actually changed shows up as a difference.
    #[test]
    fn a_narrowed_edit_serves_the_same_pictures_as_an_uncached_evaluation() {
        fn spawn() -> (EvalService, Receiver<EvalUpdate>) {
            let (tx, rx) = unbounded();
            (
                EvalService::spawn(GatedHooks, move |update| {
                    let _ = tx.send(update);
                }),
                rx,
            )
        }
        fn pixel(update: &EvalUpdate) -> f32 {
            update.results[0]
                .1
                .as_ref()
                .expect("evaluation succeeded")
                .downcast_ref::<crate::types::FrameBuffer>()
                .expect("frame buffer")
                .as_f32()[0]
        }

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let (mut cached, cached_rx) = spawn();
        let (mut reference, reference_rx) = spawn();

        let frames = 0..8u64;
        let post = |service: &mut EvalService,
                    rx: &Receiver<EvalUpdate>,
                    frame: u64,
                    document: Arc<Document>,
                    hint: InvalidationHint,
                    frame_cached: bool| {
            let mut request = frame_request(graph.clone(), node, frame, document, hint);
            if !frame_cached {
                // The reference must recompute, and dropping the *frame*
                // cache alone would not make it: the evaluator's own node
                // cache still answers, so a stale node result would be
                // compared against a stale frame and agree. `comp = None`
                // keeps the frame cache out, and `Structural` resets the
                // evaluator before every pull, so each reference answer is
                // computed from an empty cache.
                request.comp = None;
                request.hint = InvalidationHint::Structural;
            }
            service.request(request);
            pixel(&rx.recv_timeout(Duration::from_secs(5)).unwrap())
        };

        // One `Arc` for the whole fill: a fresh document per request would
        // be a fresh composition `Arc` too, and the diff would drop the
        // cache on every frame.
        let original = layered_document(1.0);
        for frame in frames.clone() {
            post(
                &mut cached,
                &cached_rx,
                frame,
                original.clone(),
                InvalidationHint::None,
                true,
            );
            post(
                &mut reference,
                &reference_rx,
                frame,
                original.clone(),
                InvalidationHint::None,
                false,
            );
        }

        // The edit: the layer's parameter, named the way the editor names it.
        let edited = layered_document(2.0);
        for frame in frames.clone() {
            let hint = InvalidationHint::Params(vec![NodeId::new(100)]);
            let served = post(
                &mut cached,
                &cached_rx,
                frame,
                edited.clone(),
                hint.clone(),
                true,
            );
            let fresh = post(
                &mut reference,
                &reference_rx,
                frame,
                edited.clone(),
                hint,
                false,
            );
            assert_eq!(
                served, fresh,
                "frame {frame}: the narrowed cache served a stale picture"
            );
        }

        // And the narrowing did happen: the layer covers `[2, 5)`, padded by
        // one frame for shutter samples, so frames 0, 6 and 7 were still
        // answered from the cache. Without this the test would also pass with
        // narrowing switched off entirely.
        assert_eq!(
            cached.frame_cache().stats().hits,
            3,
            "the frames outside the edited layer's span were recomputed"
        );
    }

    /// Cross-cache eviction routing: the frame cache and the node-result
    /// cache share the budget's pots, so an eviction list one of them is
    /// handed can name the other's entries. An id nobody drops leaves the
    /// budget counting fewer bytes than the process holds.
    #[test]
    fn a_frame_insert_evicts_node_results_through_the_worker() {
        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 0,
            // Room for a couple of 2x2 RGBA f32 frames and their node-result
            // copies, so the third request has to push something out.
            ram_bytes: 512,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        });
        let (update_tx, update_rx) = unbounded();
        let mut service = EvalService::spawn_with_budget(
            FrameHooks::new(Arc::new(AtomicUsize::new(0))),
            budget.clone(),
            move |update| {
                let _ = update_tx.send(update);
            },
        );

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .unwrap();
        let document = frame_document();
        for frame in 0..6u64 {
            service.request(frame_request(
                graph.clone(),
                node,
                frame,
                document.clone(),
                InvalidationHint::None,
            ));
            update_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        let stats = budget.stats();
        assert!(
            stats.used(crate::cache_budget::Tier::Ram) <= 512,
            "the RAM tier stayed over its limit: {stats:?}"
        );
        // What the budget believes it is holding and what the two caches
        // actually hold have to agree, or an eviction was dropped on the
        // floor by whichever cache did not own it. The evaluator holds one
        // node result or none, depending on whether the frame insert of the
        // last request pushed it out — a detail of the byte arithmetic, not
        // of the routing under test.
        let frames = service.frame_cache().stats().entries;
        assert!(
            (frames..=frames + 1).contains(&stats.entries),
            "budget entries ({}) and cached values ({frames}) disagree: {stats:?}",
            stats.entries
        );
    }
}

// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The app-wide document state and its background evaluation
//! (layer-network-model plan, Phase 3).
//!
//! [`ProjectState`] is the single owner of the live [`Document`], the
//! Document-level undo stack (REQ-LAYER-009), and the background
//! [`EvalService`]. Every document edit — timeline layer edits, node editor
//! network edits, properties shell edits — flows through
//! [`ProjectState::apply_document`] / [`ProjectState::commit_document`],
//! which swap in the new snapshot and re-request the viewer evaluation.
//!
//! The Viewer permanently evaluates the **active composition output**
//! (REQ-LAYER-007, REQ-UI-013): the shell chain is compiled with
//! deterministic ids and evaluated Document-aware, so layer networks are
//! pulled recursively by the boundary nodes. `ProjectState` is also the only
//! writer of the [`crate::panels::ActiveComposition`] global — it owns the
//! document the id must resolve in, and a switch has to drop the compiled
//! chain and re-request the evaluation.

use crate::app_settings;
use crate::panels::ViewerImage;
use crate::panels::viewer::overlay::{EvalResultKey, EvalResults, OverlayContext, OverlayRegistry};
use gpui::{App, Context, EventEmitter, Global, WeakEntity};
use ravel_core::cache_budget::SharedCacheBudget;
use ravel_core::composition::compile::{CompileError, compile_composition};
use ravel_core::composition::{AssetPath, Composition, Document, MediaAssetEntry};
use ravel_core::eval::{EvalContext, Quality};
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::{
    EvalRequest, EvalService, EvalServiceConfig, EvalUpdate, EvalWorkerHooks, InvalidationHint,
    ReadAhead, ScopedTarget,
};
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_gpu::{GpuContext, GpuFrameBuffer};
use ravel_i18n::t;
use ravel_nodes::DisplayFrame;
use ravel_project::settings::SettingsLayer;
use ravel_project::ui_state::UiState;
use ravel_ui::document::{
    CompositionSettings, DocumentStore, add_composition, add_layer_from_template, default_document,
    duplicate_composition, neighbour_composition, next_composition_name, remove_composition,
    update_composition,
};
use ravel_ui::layout_doc::LayoutDocument;
use ravel_ui::panels::timeline::BpmGrid;
use ravel_ui::panels::viewer::ViewerResolution;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// When set, [`ProjectState::new`] skips spawning the background evaluation
/// worker. gpui's deterministic test scheduler panics when a foreign OS
/// thread wakes it (even the worker's shutdown does), so test harnesses that
/// build real workspaces/panels must call
/// [`disable_background_eval_for_tests`] first.
static EVAL_DISABLED_FOR_TESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Disable the background evaluation worker for gpui tests.
pub fn disable_background_eval_for_tests() {
    EVAL_DISABLED_FOR_TESTS.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Durable registry of the app's single [`ProjectState`]. Panels resolve it
/// at construction; a stale weak entity simply fails to upgrade.
pub struct ProjectStateHandle(pub WeakEntity<ProjectState>);

impl Global for ProjectStateHandle {}

/// Durable shared state: latest per-node evaluation durations, merged across
/// background evaluations. Read by the node editor's load readout.
#[derive(Clone, Default)]
pub struct NodeEvalTimings(pub HashMap<NodeId, Duration>);

impl Global for NodeEvalTimings {}

/// One-shot project operation feedback consumed by the owning workspace.
/// Events keep UI delivery out of ProjectState and avoid a queued Global.
#[derive(Clone, Debug)]
pub enum ProjectEvent {
    GpuInitializationFailed {
        error: String,
    },
    /// The live device can no longer produce GPU evaluation frames.
    GpuDeviceLost,
    SaveFailed {
        path: PathBuf,
        error: String,
    },
    SaveChangedDuringWrite {
        path: PathBuf,
    },
    OpenFailed {
        path: PathBuf,
        error: String,
        too_new: bool,
    },
    /// A settings layer could not be persisted
    /// ([`crate::app_settings`]). Reported here because settings the user
    /// changed and Ravel then failed to keep are the same class of silent loss
    /// as a failed project save (`CRIT-02`).
    SettingsSaveFailed {
        path: PathBuf,
        error: String,
    },
    BackupRecovered {
        path: PathBuf,
        backup: PathBuf,
    },
    MediaImportSkipped {
        failures: Vec<crate::media::import::ImportFailure>,
    },
}

/// The document the session now holds came from somewhere else: a project that
/// was opened, or a new one. Carries the layout that project embedded, if any.
///
/// A separate event type from [`ProjectEvent`] because it has a separate
/// audience: `ProjectEvent`s become notifications the user reads, while this
/// one is a layout instruction only the session acts on. Emitting it as an
/// event rather than exposing state keeps `ProjectState` free of any notion of
/// the workspace arrangement.
#[derive(Clone, Debug)]
pub struct DocumentReplaced {
    /// The workspace layout the loaded project embedded, if it opted in.
    pub workspace_layout: Option<LayoutDocument>,
}

struct CompiledRoot {
    graph: Graph,
    output: NodeId,
}

/// One save request: the destination plus the document snapshot and
/// document-generation captured when the user asked for it. Queued
/// requests write what the user saw at request time, and adopt their path
/// only while the identity still matches.
struct SaveRequest {
    path: PathBuf,
    document: Document,
    /// What the user was looking at when they asked for the save
    /// (REQ-UI-013): the active composition and the Timeline's beat grid.
    /// Captured with the document so a queued save records the session it
    /// describes.
    ui_state: UiState,
    /// The workspace layout to embed, when the user opted in (DOCK-9).
    /// Captured with the document for the same reason: a queued save writes
    /// the arrangement the user asked about.
    workspace_layout: Option<LayoutDocument>,
    /// The project settings layer as it stood when the save was requested
    /// ([`crate::app_settings`] owns it). Captured with the document so a
    /// queued save writes the settings the user asked about, and so a project
    /// opened afterwards cannot leak its overrides into this archive.
    settings: SettingsLayer,
    generation: u64,
    revision: u64,
    completion: Option<SaveCompletion>,
}

type SaveCompletion = Box<dyn FnOnce(SaveOutcome, &mut App)>;

/// Result delivered to a caller waiting for one particular save request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The request saved the current document and no later edit remains.
    Saved,
    /// The request succeeded, but the document changed again while it ran.
    SavedButDirty,
    /// The request belonged to a document that has since been replaced.
    Superseded,
    /// The project archive could not be written.
    Failed,
}

/// The document a session with nothing to open starts from: the launch
/// document and the one `File ▸ New` builds.
///
/// Two settings decide it, and both are resolved here because `ravel-ui` stays
/// free of the settings layers:
///
/// - `startup.create_composition` (default on) — off means the document opens
///   with **no** composition at all, so the user picks the format in
///   `Composition ▸ New…` instead of inheriting one nobody asked for. The
///   active composition is then `None`, which is a state the session already
///   has to handle: it is what a project whose `ui_state.json` names nothing
///   resolves to (`UiState::initial_active_comp`), and every panel renders its
///   empty form.
/// - `playback.frame_rate` (`SET-6`) — the root composition of a fresh
///   document has nothing to inherit a format from, so it takes the resolved
///   default rate. The settings global is installed before the first window
///   exists (`crate::main`), so this reads the user's value; a tool that
///   builds a `ProjectState` without the bootstrap gets the built-in one.
fn fresh_document(cx: &App) -> Document {
    if app_settings::resolved(cx).startup_creates_composition {
        default_document(app_settings::default_frame_rate(cx))
    } else {
        Document::default()
    }
}

/// GPUI entity owning the document, its undo history, and the background
/// evaluation service.
pub struct ProjectState {
    store: DocumentStore,
    /// Shared so an overlay snapshot can carry the parameter declarations
    /// without copying every template on each pointer move.
    registry: Arc<NodeRegistry>,
    /// Background evaluation worker; owns the Evaluator, GpuContext, and
    /// ShaderManager so the UI thread never blocks on evaluation. `None`
    /// only in tests (a live worker thread breaks the deterministic gpui
    /// test scheduler).
    eval: Option<EvalService>,
    /// The device the evaluation worker runs on, retained so a **second**
    /// worker can be built on it.
    ///
    /// The render queue (`render-export-plan.md`, unit 5) needs its own
    /// [`EvalWorkerHooks`] — sharing the interactive service's would put
    /// export frames and preview frames in one cache, which is the coupling
    /// that worker exists to avoid — but it must not open a second adapter:
    /// REQ-GPU-001 puts the whole pipeline on one device, and `GpuContext` is
    /// cheap to clone precisely so a second consumer shares it.
    gpu: Option<GpuContext>,
    /// Whether the session has already announced its GPU device loss.
    ///
    /// Multiple frames can observe the same loss, but the workspace must show
    /// one durable notification only.
    gpu_loss_notified: bool,
    /// Host capability shared with the evaluation worker. It is false until
    /// the live GPUI window proves that both renderers use the same device;
    /// false also selects the worker-side CPU fallback on unsupported hosts.
    viewer_surface_enabled: Arc<AtomicBool>,
    /// The cache budget the evaluation worker answers to, retained for the
    /// same reason: the render worker's evaluator gets a clone, so both
    /// answer to one authority rather than two independent limits
    /// (`cache-plan.md`, `CACHE-3`).
    ///
    /// Unconditional, unlike `eval` and `gpu`: accounting needs no adapter,
    /// and a session whose budget only exists on a machine with a GPU would
    /// leave the settings that move it (`SET-8`) with nothing to apply to
    /// exactly where that wiring is checked.
    cache_budget: SharedCacheBudget,
    /// GPU initialization failure captured at startup. The workspace shows it
    /// after its Root exists, so adapter-less systems get a visible error
    /// instead of a constructor panic.
    startup_gpu_error: Option<String>,
    /// Compiled shell chain of the active composition, rebuilt after every
    /// document change and every composition switch (deterministic ids keep
    /// the evaluator caches warm).
    compiled: Option<CompiledRoot>,
    /// Invalidation accumulated while no request could be posted (e.g. an
    /// empty composition). Merged into the next posted request so a
    /// structural change is never lost.
    pending_hint: InvalidationHint,
    /// Path of the currently open `.ravprj`, set after a successful save or
    /// load; `None` for a never-saved document.
    project_path: Option<PathBuf>,
    /// Document identity counter, bumped when the document is replaced
    /// wholesale (new/load). An async save adopts its path only while the
    /// identity is unchanged — a path must never leak onto an unrelated
    /// replacement document.
    generation: u64,
    /// Document mutation counter, bumped by every user-driven store
    /// mutation (edits, undo/redo, New). An async load applies its result
    /// only while no intervening mutation happened — user edits must never
    /// be silently discarded. Load applications themselves do not bump it:
    /// a pending newer load must not be invalidated by an older one.
    revision: u64,
    /// Revision captured by the most recent successful save of this document.
    /// This advances on save completion, not request: a save writes its
    /// request-time snapshot, so edits made while it runs must remain dirty.
    /// New and loaded documents reset this to their current revision.
    saved_revision: u64,
    /// Whether an async save is currently in flight; a save requested
    /// while one runs is queued in `pending_saves` and started on
    /// completion, so writes never reach the disk out of order.
    save_in_flight: bool,
    /// Queued save requests, oldest first (see `save_in_flight`).
    pending_saves: std::collections::VecDeque<SaveRequest>,
    /// Monotonic load-request counter; only the newest load may apply
    /// (latest-wins for overlapping File ▸ Open requests).
    load_request: u64,
    /// Generation of everything the document-mirroring panels display:
    /// document content plus the composition the UI edits. Bumped by every
    /// document change and every composition switch, and by nothing else — a
    /// notify that leaves it unchanged (save completion, which only moves the
    /// window title) means no panel has anything to rebuild, so each one
    /// compares this against the epoch it last synced and returns early.
    ///
    /// Deliberately separate from `revision`, which answers a different
    /// question (may this async load still apply?) and is therefore *not*
    /// bumped when a load replaces the document — a panel gate keyed on it
    /// would leave the whole workspace showing the previous project.
    mirror_epoch: u64,
    /// Eval generation of the currently displayed [`ViewerFrame`]. An
    /// arriving update is published only when it is newer, so results
    /// always move the display forward; direct blanks (empty composition,
    /// compile error) advance this to the post-`cancel_pending` generation
    /// so an in-flight older result cannot overwrite them.
    published_generation: u64,
    /// `SharedFrameCache::version()` the Timeline's cache band was last
    /// computed at, so an evaluation that changed nothing skips the walk.
    published_band_version: Option<u64>,
    /// Bumped only by changes that can add or remove nodes: a `Structural`
    /// document change, a document replacement, and a composition switch.
    ///
    /// Deliberately narrower than `mirror_epoch`, which moves for *every*
    /// document change — a scrub drag bumps that one on every mouse move,
    /// and re-scanning the whole document for node ids at that rate would
    /// put a new cost on the UI thread. Any edit that adds or removes a node
    /// passes `InvalidationHint::Structural`, so this is the exact gate for
    /// [`Self::live_nodes`].
    structure_epoch: u64,
    /// Node ids reachable from the document, cached against the
    /// `structure_epoch` it was scanned at. Used to keep [`NodeEvalTimings`]
    /// from outgrowing the document (see [`Self::prune_eval_timings`]).
    live_nodes: HashSet<NodeId>,
    /// Epoch [`Self::live_nodes`] was scanned at; `None` before the first
    /// scan (epoch 0 is a real document).
    live_nodes_epoch: Option<u64>,
    /// Fraction of the composition resolution the viewer evaluates at
    /// (REQ-UI-004). View state, not document content: it is never written to
    /// the `.ravprj`, so opening somebody else's project does not import
    /// their preview setting.
    viewer_resolution: ViewerResolution,
}

/// Every node id the document can evaluate: the flat graph, every layer
/// network of every composition, and the subnets nested inside them.
/// Mirrors the traversal of [`Document::id_watermarks`].
fn document_node_ids(document: &Document) -> HashSet<NodeId> {
    fn scan_graph(graph: &Graph, ids: &mut HashSet<NodeId>) {
        for node in graph.nodes() {
            ids.insert(node.id);
            if let Some(subnet) = &node.subnet {
                scan_graph(subnet, ids);
            }
        }
    }

    let mut ids = HashSet::new();
    scan_graph(&document.graph, &mut ids);
    for comp in document.compositions.values() {
        for layer in &comp.layers {
            scan_graph(&layer.network, &mut ids);
        }
    }
    ids
}

/// What one completed evaluation produced for the viewer.
enum ViewerOutput {
    /// A frame, already converted to the display image.
    Image(ViewerImage),
    /// A display-encoded frame whose texture is ready for GPUI to sample
    /// without a CPU round trip.
    Gpu(GpuFrameBuffer),
    /// The output is not a displayable frame (a `Scalar`, or a frame with
    /// degenerate dimensions): the viewer blanks.
    NotAFrame,
    /// The evaluation failed; the message becomes the viewer's error overlay.
    Failed(String),
}

/// One evaluation result on its way to the UI, with the display conversion
/// already applied.
///
/// Built from an [`EvalUpdate`] **on the evaluation worker thread** (see
/// [`spawn_viewer_eval_service`]). The f32→BGRA conversion of the frame used
/// to run in the Viewer's `ViewerFrame` observer, i.e. on the UI thread, once
/// per played or scrubbed frame (issue HIGH-08); the frame itself was also
/// cloned there. Converting before the hop leaves the UI thread an `Arc` move.
pub(crate) struct ViewerUpdate {
    generation: u64,
    frame: u64,
    timings: Vec<(NodeId, Duration)>,
    output: ViewerOutput,
    scoped_results: Vec<(EvalResultKey, Arc<dyn ravel_core::types::NodeData>)>,
}

/// Whether an evaluation result is an image the viewer would have drawn had
/// the display transform run. Both frame representations count: the worker
/// hands back whichever one it was holding when the transform failed.
fn frame_shaped(value: &dyn ravel_core::types::NodeData) -> bool {
    value.downcast_ref::<FrameBuffer>().is_some() || value.is_gpu_resident()
}

impl ViewerUpdate {
    /// Convert an evaluation result for display. Call sites: the worker
    /// callback below, and tests that drive [`ProjectState::on_eval_update`]
    /// directly.
    /// The viewer reads the request's **first** target, which
    /// [`ProjectState::build_viewer_request`] always fills with the
    /// composition output; further targets exist for inspection panels and
    /// are not the viewer's business. A result-less update cannot arise from
    /// a request this crate builds, so it blanks rather than erroring.
    ///
    /// Overlay values come from `scoped`, which is where the request put
    /// them, and keep the scope they were evaluated in as part of their key.
    pub(crate) fn from_eval(update: EvalUpdate) -> Self {
        let output = match update.results.into_iter().next() {
            // The worker's hooks finish a viewer frame on the GPU (`CM-7`), so
            // what arrives is display bytes rather than a linear buffer.
            Some((_, Ok(data))) => match data.downcast_ref::<DisplayFrame>() {
                Some(frame) => match frame.gpu_frame() {
                    Some(gpu) => ViewerOutput::Gpu(gpu.clone()),
                    None => match ViewerImage::from_display_frame(frame) {
                        Some(image) => ViewerOutput::Image(image),
                        // A degenerate frame carries nothing to draw; the
                        // panel used to receive it as a `Frame` whose image
                        // was `None` and paint the same black quad it paints
                        // for `Blank`.
                        None => ViewerOutput::NotAFrame,
                    },
                },
                // A frame that is still linear means `finalize` could not run
                // the display transform — a shader that will not compile, a
                // lost device. Drawing linear light would be wrong and
                // blanking would be silent, so say what happened. Anything
                // that is not a frame at all (a `Scalar`) still blanks.
                None if frame_shaped(data.as_ref()) => {
                    ViewerOutput::Failed(t!("viewer.display_transform_failed"))
                }
                None => ViewerOutput::NotAFrame,
            },
            Some((_, Err(err))) => ViewerOutput::Failed(err.to_string()),
            None => ViewerOutput::NotAFrame,
        };
        let scoped_results = update
            .scoped
            .into_iter()
            .filter_map(|scoped| {
                scoped
                    .output
                    .ok()
                    .map(|value| ((scoped.path, scoped.node), value))
            })
            .collect();
        Self {
            generation: update.generation,
            frame: update.frame,
            timings: update.timings,
            output,
            scoped_results,
        }
    }
}

/// Spawn the evaluation worker with the viewer's display conversion attached
/// to its result callback.
///
/// `EvalService` invokes that callback **on the worker thread**, which is why
/// the conversion belongs in it: the result crosses into display form before
/// it is handed to the UI thread. Named, and generic over the hooks, so a test
/// can drive the production wiring with stub hooks and assert which thread the
/// conversion ran on.
fn spawn_viewer_eval_service<H: EvalWorkerHooks>(
    hooks: H,
    budget: SharedCacheBudget,
    updates: futures::channel::mpsc::UnboundedSender<ViewerUpdate>,
) -> EvalService {
    // Read-ahead is on here and nowhere else (`CACHE-9`): this is the one
    // service with a playhead a user moves, so it is the one worth filling
    // ahead of. A render and a benchmark evaluate exactly the frames they
    // name.
    let config = EvalServiceConfig {
        budget: Some(budget),
        read_ahead: Some(ReadAhead::default()),
    };
    EvalService::spawn_with_config(hooks, config, move |update| {
        let _ = updates.unbounded_send(ViewerUpdate::from_eval(update));
    })
}

impl ProjectState {
    /// Build the session on a GPU device Ravel chooses for itself.
    ///
    /// Prefer [`Self::new_on_host_gpu`] from the application: a device the
    /// window renderer already owns is the one REQ-GPU-001 asks for, and the
    /// only one whose textures the viewer can hand back without a copy.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::new_on_host_gpu(None, cx)
    }

    /// Build the session on `host_gpu` when the window renderer supplied one.
    ///
    /// `None` falls back to Ravel's own adapter selection — the headless
    /// tests, and any platform whose renderer is not wgpu-backed.
    ///
    /// **Adopting matters even where both sides would pick the same card.**
    /// `Backends::PRIMARY` is ordered `VULKAN | METAL | DX12 | …`, so on a
    /// Windows machine with an NVIDIA driver Ravel lands on Vulkan while
    /// GPUI's renderer is on DX12 — one GPU, two devices, and a texture from
    /// either is meaningless to the other. Taking the host's device removes
    /// the choice instead of trying to make two choices agree.
    pub fn new_on_host_gpu(host_gpu: Option<GpuContext>, cx: &mut Context<Self>) -> Self {
        // The one place a `CacheBudget` is created. The texture pool is built
        // inside `GpuEvalHooks`, before the worker thread that builds the
        // `Evaluator` exists, so both have to be handed the same budget from
        // here — that is what "one authority" means in practice
        // (`cache-plan.md`, `CACHE-3`).
        //
        // The global layer is installed before the first window opens
        // (`main`), so its `[cache]` limits are in force from the first
        // reservation rather than after a correction. The project layer
        // arrives later, with the document — that one, and every preferences
        // edit, reach the budget through `app_settings::apply_cache_budget`
        // (`SET-8`).
        let cache_budget = SharedCacheBudget::new(app_settings::resolved(cx).cache_budget());
        let viewer_surface_enabled = Arc::new(AtomicBool::new(false));
        let (eval, gpu, startup_gpu_error) =
            if EVAL_DISABLED_FOR_TESTS.load(std::sync::atomic::Ordering::SeqCst) {
                (None, None, None)
            } else {
                match host_gpu.map_or_else(GpuContext::new_blocking, Ok) {
                    Ok(gpu_ctx) => {
                        let (update_tx, mut update_rx) =
                            futures::channel::mpsc::unbounded::<ViewerUpdate>();
                        let budget = cache_budget.clone();
                        let eval = spawn_viewer_eval_service(
                            // The viewer is the one worker whose frames go to a
                            // screen, so it is the one that finishes them on the
                            // GPU (`CM-7`). The export worker deliberately does
                            // not: its own encode step needs the linear frame.
                            ravel_nodes::GpuEvalHooks::with_budget(gpu_ctx.clone(), budget.clone())
                                .with_display_surface_mode(viewer_surface_enabled.clone()),
                            budget.clone(),
                            update_tx,
                        );
                        cx.spawn(async move |this, cx| {
                            use futures::StreamExt as _;
                            while let Some(update) = update_rx.next().await {
                                if this
                                    .update(cx, |this, cx| this.on_eval_update(update, cx))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        })
                        .detach();
                        (Some(eval), Some(gpu_ctx), None)
                    }
                    Err(error) => {
                        tracing::error!(%error, "GPU context initialization failed");
                        (None, None, Some(error.to_string()))
                    }
                }
            };

        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let registry = Arc::new(registry);

        let store = DocumentStore::new(fresh_document(cx));
        // The startup document opens on its root composition; from here on
        // the active composition is UI state, never written back to the
        // document (REQ-UI-013).
        crate::panels::set_active_composition(store.document().root_comp, cx);

        Self {
            store,
            registry,
            eval,
            gpu,
            gpu_loss_notified: false,
            viewer_surface_enabled,
            cache_budget,
            startup_gpu_error,
            compiled: None,
            pending_hint: InvalidationHint::None,
            project_path: None,
            generation: 0,
            revision: 0,
            saved_revision: 0,
            save_in_flight: false,
            pending_saves: std::collections::VecDeque::new(),
            load_request: 0,
            mirror_epoch: 0,
            published_generation: 0,
            published_band_version: None,
            structure_epoch: 0,
            live_nodes: HashSet::new(),
            live_nodes_epoch: None,
            viewer_resolution: ViewerResolution::default(),
        }
    }

    pub fn startup_gpu_error(&self) -> Option<&str> {
        self.startup_gpu_error.as_deref()
    }

    /// The device the evaluation worker runs on, for a second worker that
    /// must share it (the render queue). `None` when there is no adapter, or
    /// in tests, which is the same condition as `eval` being absent.
    pub fn gpu_context(&self) -> Option<&GpuContext> {
        self.gpu.as_ref()
    }

    /// Report a device-loss observation from an existing update path.
    ///
    /// `detected` is used by the adopted-host path, where GPUI owns the loss
    /// callback and Ravel detects the device identity change at the Viewer
    /// surface guard. Self-owned contexts additionally consult their shared
    /// [`GpuContext`] state. The event is emitted once per session.
    pub fn report_gpu_device_loss(&mut self, detected: bool, cx: &mut Context<Self>) {
        let self_owned_loss = self.gpu.as_ref().is_some_and(GpuContext::lost);
        if (!detected && !self_owned_loss) || self.gpu_loss_notified {
            return;
        }
        self.gpu_loss_notified = true;
        cx.emit(ProjectEvent::GpuDeviceLost);
    }

    /// Configure whether the live GPUI host can sample the worker's output
    /// texture directly. A change requests one fresh viewer frame so an old
    /// CPU/GPU representation is not kept after a window/device transition.
    pub fn configure_viewer_surface(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.report_gpu_device_loss(false, cx);
        if self.viewer_surface_enabled.swap(enabled, Ordering::Release) == enabled {
            return;
        }
        tracing::info!(enabled, "viewer GPU surface capability changed");
        self.request_viewer_eval(InvalidationHint::None, cx);
        cx.notify();
    }

    /// The cache budget the evaluation worker answers to; see
    /// [`Self::cache_budget`](Self::cache_budget) on the field.
    pub fn cache_budget(&self) -> &SharedCacheBudget {
        &self.cache_budget
    }

    /// Generation of what the document-mirroring panels display; see
    /// [`Self::mirror_epoch`]. A panel that has already synced this epoch has
    /// nothing to rebuild.
    pub fn mirror_epoch(&self) -> u64 {
        self.mirror_epoch
    }

    pub fn document(&self) -> &Document {
        self.store.document()
    }

    /// Path of the currently open `.ravprj`, if the document was saved or
    /// loaded.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    /// Whether the live document has changes newer than its last completed
    /// save (or its New/load baseline).
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// The same registry as a shared handle, for the overlay snapshot.
    pub fn shared_registry(&self) -> Arc<NodeRegistry> {
        self.registry.clone()
    }

    /// The composition the UI is editing, resolved in the live document.
    /// `None` when nothing is active or the active id is not in this
    /// document (composition 0).
    pub fn active_composition(&self, cx: &App) -> Option<&Composition> {
        crate::panels::active_composition_in(self.store.document(), cx)
    }

    /// Switch the composition the UI edits (REQ-UI-013). The layer selection
    /// is reset with it, the compiled chain is dropped, and the viewer
    /// re-evaluates. The document is untouched: `root_comp` keeps naming the
    /// composition a reopened document starts on, so a switch lands in
    /// neither the undo history nor the saved file.
    pub fn set_active_composition(&mut self, comp: Option<CompId>, cx: &mut Context<Self>) {
        if crate::panels::active_composition(cx) == comp {
            return;
        }
        crate::panels::set_active_composition(comp, cx);
        self.compiled = None;
        self.mirror_epoch += 1;
        // The document is untouched, so the node set cannot have moved; the
        // bump keeps "which nodes can be evaluated from here" and the live
        // set in step anyway, and a switch is far too rare for the sweep to
        // matter.
        self.structure_epoch += 1;
        crate::audio::sync_from_document(self.store.document(), cx);
        self.request_viewer_eval(InvalidationHint::Structural, cx);
        cx.notify();
    }

    // ----- document edits ----------------------------------------------------

    /// Live (mid-gesture) document update: no undo step is recorded.
    pub fn apply_document(
        &mut self,
        doc: Document,
        hint: InvalidationHint,
        cx: &mut Context<Self>,
    ) {
        self.revision += 1;
        self.store.apply(doc);
        self.document_changed(hint, cx);
    }

    /// Committed document update: records one undo step.
    pub fn commit_document(
        &mut self,
        doc: Document,
        hint: InvalidationHint,
        cx: &mut Context<Self>,
    ) {
        self.revision += 1;
        self.store.commit(doc);
        self.document_changed(hint, cx);
    }

    /// Discard uncommitted live edits (cancelled gestures), restoring the
    /// last committed snapshot. Returns whether anything changed.
    pub fn revert_document(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.revert();
        if changed {
            self.revision += 1;
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    /// Cancel a gesture against the snapshot it captured at begin time,
    /// removing any later commit that accidentally included its preview.
    pub fn restore_document_snapshot(
        &mut self,
        snapshot: Document,
        cx: &mut Context<Self>,
    ) -> bool {
        let changed = self.store.restore_snapshot(snapshot);
        if changed {
            self.revision += 1;
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    /// Document-level undo (REQ-LAYER-009). Returns whether a step was taken.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.undo();
        if changed {
            self.revision += 1;
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    /// Document-level redo. Returns whether a step was taken.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.redo();
        if changed {
            self.revision += 1;
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    // ----- project file (`.ravprj`) -------------------------------------------

    /// Replace the document with a fresh default one (File ▸ New). The undo
    /// history and project path are reset along with the document.
    pub fn new_document(&mut self, cx: &mut Context<Self>) {
        // A new project overrides nothing: the previous project's settings must
        // stop applying with it — and they have to stop *before* the document is
        // built, because the new root composition takes the default frame rate
        // from the settings in force (`SET-6`). Dropping the layer here rather
        // than only in `replace_document` is what keeps the closing project's
        // frame rate out of the project that replaces it; the call below then
        // finds the layer already empty and does nothing.
        app_settings::set_project_layer(SettingsLayer::default(), cx);
        // A user-driven replacement: invalidates in-flight loads.
        let document = fresh_document(cx);
        self.revision += 1;
        self.replace_document(
            document,
            None,
            &UiState::default(),
            SettingsLayer::default(),
            cx,
        );
        cx.emit(DocumentReplaced {
            workspace_layout: None,
        });
    }

    /// Save the current document as a `.ravprj` at `path` (File ▸ Save /
    /// Save As). The document snapshot is cloned cheaply (`im` structural
    /// sharing) **at request time** and travels with the request, so a
    /// queued save writes the document the user asked about, not whatever
    /// is current when it starts. RON encoding, zip packing, and the file
    /// write all run on the background executor so the UI thread never
    /// blocks. `project_path` is updated only on success. Saves requested
    /// while another is in flight are queued and run in request order, so
    /// writes never land out of order.
    /// `workspace_layout` is the opt-in arrangement to embed, which the caller
    /// resolves (`crate::layout_persist::document_for_embedding`) because only
    /// the session owns the live layout.
    pub fn save_project_to(
        &mut self,
        path: PathBuf,
        workspace_layout: Option<LayoutDocument>,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_save(path, workspace_layout, None, cx);
    }

    /// Save and notify `completion` when this specific request finishes.
    /// Requests made during another save retain FIFO order, so the callback
    /// cannot run until all earlier queued saves have completed.
    pub fn save_project_to_then(
        &mut self,
        path: PathBuf,
        workspace_layout: Option<LayoutDocument>,
        completion: impl FnOnce(SaveOutcome, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_save(path, workspace_layout, Some(Box::new(completion)), cx);
    }

    fn enqueue_save(
        &mut self,
        path: PathBuf,
        workspace_layout: Option<LayoutDocument>,
        completion: Option<SaveCompletion>,
        cx: &mut Context<Self>,
    ) {
        let request = SaveRequest {
            path,
            document: self.store.document().clone(),
            ui_state: UiState {
                active_comp: crate::panels::active_composition(cx),
                // An untouched grid writes no entry, so saving a project
                // nobody used the beat grid in leaves `ui_state.json`
                // byte-identical to what earlier builds wrote.
                bpm_grid: Some(crate::panels::bpm_grid(cx))
                    .filter(|grid| *grid != BpmGrid::default()),
                // Same rule for the loop ranges: no entry at all until one
                // composition actually has one.
                loop_ranges: crate::panels::loop_ranges(cx).into_iter().collect(),
            },
            workspace_layout,
            settings: crate::app_settings::layer(crate::app_settings::SettingsScope::Project, cx),
            generation: self.generation,
            revision: self.revision,
            completion,
        };
        if self.save_in_flight {
            self.pending_saves.push_back(request);
            return;
        }
        self.save_in_flight = true;
        self.spawn_save(request, cx);
    }

    /// Run one save; the caller holds `save_in_flight`.
    fn spawn_save(&mut self, request: SaveRequest, cx: &mut Context<Self>) {
        let SaveRequest {
            path,
            document,
            ui_state,
            workspace_layout,
            settings,
            generation,
            revision,
            completion,
        } = request;
        let write_path = path.clone();
        let write = cx.background_executor().spawn(async move {
            // Overwriting an existing project keeps its original creation
            // timestamp; anything unreadable falls back to now.
            let created_at = ravel_project::read_created_at(&write_path)
                .unwrap_or_else(ravel_project::timestamp::rfc3339_now);
            let project_name = write_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".to_string());
            let mut file =
                ravel_project::ProjectFile::from_document(project_name, created_at, document);
            file.manifest.modified_at = ravel_project::timestamp::rfc3339_now();
            file.settings = settings;
            file.ui_state = ui_state;
            // `None` while the opt-in is off, which leaves the archive without
            // the entry at all.
            file.workspace_layout = workspace_layout;
            file.save(&write_path)
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            let _ = this.update(cx, |this, cx| {
                let outcome = match result {
                    Ok(()) => {
                        // Adopt the path only while the document identity is
                        // unchanged since the request: a New/Open during the
                        // write must not inherit a path that describes
                        // different content.
                        if this.generation == generation {
                            this.project_path = Some(path.clone());
                            this.saved_revision = revision;
                            if this.revision == revision {
                                SaveOutcome::Saved
                            } else {
                                cx.emit(ProjectEvent::SaveChangedDuringWrite {
                                    path: path.clone(),
                                });
                                SaveOutcome::SavedButDirty
                            }
                        } else {
                            tracing::warn!(
                                path = %path.display(),
                                "save finished after the document was replaced; path not adopted"
                            );
                            SaveOutcome::Superseded
                        }
                    }
                    Err(err) => {
                        tracing::error!(%err, path = %path.display(), "failed to save project");
                        cx.emit(ProjectEvent::SaveFailed {
                            path: path.clone(),
                            error: err.to_string(),
                        });
                        SaveOutcome::Failed
                    }
                };
                this.save_in_flight = false;
                if let Some(next) = this.pending_saves.pop_front() {
                    this.save_in_flight = true;
                    this.spawn_save(next, cx);
                }
                if let Some(completion) = completion {
                    // Run after this entity update ends: replacement callbacks
                    // may update ProjectState again through the workspace.
                    cx.defer(move |cx| completion(outcome, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Load a `.ravprj` from `path`, replacing the current document (File ▸
    /// Open). The file read runs on the background executor; loading is not
    /// an undo step (the store and its history are replaced wholesale).
    /// Latest-wins for overlapping requests, and the result is discarded
    /// when the user edited (or replaced) the document while the read was
    /// in flight — edits are never silently lost.
    pub fn load_project_from(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load_request += 1;
        let request = self.load_request;
        let revision = self.revision;
        let read = cx.background_executor().spawn({
            let path = path.clone();
            async move { ravel_project::ProjectFile::load_with_backup(&path) }
        });
        cx.spawn(async move |this, cx| match read.await {
            Ok(loaded) => {
                let _ = this.update(cx, |this, cx| {
                    if this.load_request == request && this.revision == revision {
                        let file = loaded.project;
                        let workspace_layout = file.workspace_layout;
                        this.replace_document(
                            file.document,
                            Some(path),
                            &file.ui_state,
                            file.settings,
                            cx,
                        );
                        cx.emit(DocumentReplaced { workspace_layout });
                        if let Some(backup) = loaded.recovered_from {
                            cx.emit(ProjectEvent::BackupRecovered {
                                path: this.project_path.clone().unwrap_or_default(),
                                backup,
                            });
                        }
                    } else {
                        tracing::warn!(
                            path = %path.display(),
                            "discarding loaded project: superseded or edited while reading"
                        );
                    }
                });
            }
            Err(err) => {
                tracing::error!(%err, path = %path.display(), "failed to load project");
                let _ = this.update(cx, |this, cx| {
                    if this.load_request == request && this.revision == revision {
                        cx.emit(ProjectEvent::OpenFailed {
                            path,
                            error: err.to_string(),
                            too_new: err.is_too_new(),
                        });
                    }
                });
            }
        })
        .detach();
    }

    // ----- settings (`SET-1`) -------------------------------------------------

    /// Record that the project settings layer changed, so the project counts
    /// as having unsaved changes.
    ///
    /// The layer itself lives in [`crate::app_settings`]; what belongs here is
    /// only the consequence — the `settings.toml` entry of the open `.ravprj`
    /// no longer matches the file on disk, and the next save writes it
    /// (`enqueue_save` captures the layer). Deliberately not a document edit:
    /// settings are not part of the `Document`, so this records no undo step
    /// and moves no panel-rebuild epoch.
    pub fn mark_settings_changed(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        cx.notify();
    }

    /// Report a settings write that failed, so the user hears about it
    /// ([`ProjectEvent::SettingsSaveFailed`]).
    ///
    /// `ProjectEvent` is the session's user-visible feedback channel, and
    /// every emission of it lives in this module; the settings writer calls
    /// this rather than emitting on this entity from outside.
    pub fn report_settings_write_failure(
        &mut self,
        path: PathBuf,
        error: String,
        cx: &mut Context<Self>,
    ) {
        cx.emit(ProjectEvent::SettingsSaveFailed { path, error });
    }

    /// Swap in a whole new document (new project / loaded project): fresh
    /// undo history, dropped compile cache and stale invalidation, and one
    /// structural viewer re-evaluation. Bumps `generation` only — the
    /// caller is responsible for `revision` when the replacement comes from
    /// a user action, so load applications do not invalidate pending newer
    /// loads.
    ///
    /// `active_comp` is the composition the replacement opens on: the
    /// document root for a new project, the restored `ui_state.json` entry
    /// for a loaded one (REQ-UI-013). `bpm_grid` follows the same rule, so a
    /// project's beat grid stops applying as it is replaced instead of
    /// leaking into the next project.
    ///
    /// `settings` is the project's own settings layer, which is adopted here
    /// so a project's overrides start applying as it opens and stop applying
    /// as it is replaced. This is the only place the project layer is
    /// installed ([`crate::app_settings::set_project_layer`]).
    fn replace_document(
        &mut self,
        document: Document,
        path: Option<PathBuf>,
        ui_state: &UiState,
        settings: SettingsLayer,
        cx: &mut Context<Self>,
    ) {
        // Everything the UI opens the project on comes out of one entry, so
        // adding a field here is one call site rather than one parameter.
        let active_comp = ui_state.initial_active_comp(&document);
        let bpm_grid = ui_state.bpm_grid();
        let loop_ranges = ui_state.loop_ranges(&document);
        // The layer selection of the previous document never carries over —
        // even a reloaded project reuses composition ids for different
        // content. Published after the swap so observers resolve the new id
        // in the document that actually holds it.
        self.store = DocumentStore::new(document);
        crate::panels::set_active_composition(active_comp, cx);
        crate::panels::set_bpm_grid(bpm_grid, cx);
        crate::panels::set_loop_ranges(loop_ranges, cx);
        crate::app_settings::set_project_layer(settings, cx);
        self.project_path = path;
        self.generation += 1;
        self.saved_revision = self.revision;
        self.compiled = None;
        // A wholesale replacement changes everything every panel mirrors, and
        // `revision` deliberately does not move here (see its doc comment), so
        // the panel gate needs its own bump.
        self.mirror_epoch += 1;
        self.structure_epoch += 1;
        self.pending_hint = InvalidationHint::None;
        // Asset ids may be reused for different files across documents:
        // drop the audio cache/tracks before the first sync of the new one.
        crate::audio::document_replaced(cx);
        // Node ids are reused across documents too (a persisted id is just a
        // number, and `advance_id_counters` knows nothing of the document
        // being replaced), so a measurement carried over would be attached to
        // a different node of the new document. Pruning cannot catch that —
        // the id is live in both — so the readouts start empty.
        if cx.has_global::<NodeEvalTimings>() {
            cx.set_global(NodeEvalTimings::default());
        }
        crate::audio::sync_from_document(self.store.document(), cx);
        self.request_viewer_eval(InvalidationHint::Structural, cx);
        cx.notify();
    }

    /// Create a layer from a builtin template on top of the active
    /// composition's stack (REQ-LAYER-008).
    pub fn add_layer_from_template(
        &mut self,
        template_key: &str,
        cx: &mut Context<Self>,
    ) -> Option<LayerId> {
        let comp = self.active_composition(cx)?.id;
        let Some(template) =
            ravel_core::composition::templates::builtin_layer_template(template_key)
        else {
            tracing::warn!(template_key, "unknown layer template");
            return None;
        };
        match add_layer_from_template(self.store.document(), comp, template, &self.registry) {
            Ok(Some((doc, layer_id))) => {
                self.commit_document(doc, InvalidationHint::Structural, cx);
                Some(layer_id)
            }
            Ok(None) => None,
            Err(err) => {
                tracing::error!(%err, template_key, "layer template instantiation failed");
                None
            }
        }
    }

    // ----- media import (REQ-UI-010) -------------------------------------------

    /// Apply one batch of probed media files to the document (File ▸ Import
    /// / OS file drop): register each as a media asset, reusing the existing
    /// entry when the same absolute path is already present. The whole batch
    /// is a single `commit_document`, i.e. one undo step.
    ///
    /// **Import only imports** (refactor unit 10): nothing is placed on a
    /// composition. Putting an asset on the timeline is its own action —
    /// the MediaBin's Add as Layer, a double click, or a drag onto the
    /// Timeline or Viewer, all of which land in [`Self::add_asset_layers`].
    ///
    /// Probing happened before this call (background executor, see
    /// [`crate::media::import`]); this method is the synchronous document
    /// edit. Composition settings are never touched (decision 5).
    pub fn import_media(
        &mut self,
        probed: Vec<crate::media::import::ProbedAsset>,
        skipped: Vec<crate::media::import::ImportFailure>,
        cx: &mut Context<Self>,
    ) -> crate::media::import::ImportSummary {
        let mut summary = crate::media::import::ImportSummary {
            skipped,
            ..crate::media::import::ImportSummary::default()
        };
        if !summary.skipped.is_empty() {
            cx.emit(ProjectEvent::MediaImportSkipped {
                failures: summary.skipped.clone(),
            });
        }
        if probed.is_empty() {
            return summary;
        }

        let project_root = self
            .project_path
            .as_deref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf);

        let mut doc = self.store.document().clone();
        // Dedupe within the batch as well as against the document: two
        // frames of one sequence (or the same file picked twice) resolve to
        // one asset.
        let mut batch_ids: HashMap<PathBuf, String> = HashMap::new();
        // Whether the run put anything new in the document. An import that
        // only names paths already in the bin leaves it untouched, and
        // committing that would push an undo step that reverts nothing —
        // reachable only since placing stopped being part of an import.
        let mut registered = false;
        for asset in probed {
            let id = match batch_ids.get(&asset.path).cloned().or_else(|| {
                doc.media_assets.iter().find_map(|(id, entry)| {
                    (entry.resolved.as_deref() == Some(asset.path.as_path())).then(|| id.clone())
                })
            }) {
                Some(id) => id,
                None => {
                    registered = true;
                    let id = unique_asset_id(&doc, &asset.path);
                    doc = doc.with_media_asset_entry(
                        id.clone(),
                        MediaAssetEntry {
                            color_space: None,
                            path: AssetPath::for_project_root(&asset.path, project_root.as_deref()),
                            kind: asset.kind.clone(),
                            metadata: asset.metadata.clone(),
                            resolved: Some(asset.path.clone()),
                        },
                    );
                    id
                }
            };
            batch_ids.insert(asset.path.clone(), id.clone());
            summary.imported.push((id, asset.path.clone()));
        }

        if registered {
            self.commit_document(doc, InvalidationHint::Structural, cx);
        }
        summary
    }

    /// Stack a layer for each already-imported asset on the active
    /// composition, starting at `start_frame`, and return the new layer ids.
    ///
    /// The whole batch is one `commit_document`, so dropping five clips onto
    /// the Timeline at once is **one** undo step. A no-op without an active
    /// composition or when nothing resolves.
    pub fn add_asset_layers(
        &mut self,
        asset_ids: &[String],
        start_frame: i64,
        cx: &mut Context<Self>,
    ) -> Vec<LayerId> {
        let Some(comp) = self.active_composition(cx).map(|comp| comp.id) else {
            tracing::warn!("add as layer: no active composition");
            return Vec::new();
        };
        let (doc, layers) = ravel_ui::document::add_media_layers(
            self.store.document(),
            comp,
            &self.registry,
            asset_ids,
            start_frame,
        );
        if layers.is_empty() {
            return layers;
        }
        self.commit_document(doc, InvalidationHint::Structural, cx);
        layers
    }

    /// The frame a "put this asset on the timeline" action places at when it
    /// has no position of its own (the menu item, the double click).
    pub fn playhead_frame(cx: &App) -> i64 {
        cx.try_global::<crate::panels::PlaybackPosition>()
            .map(|position| position.frame as i64)
            .unwrap_or(0)
    }

    // ----- composition management (REQ-UI-013) --------------------------------

    /// The settings `Composition ▸ New…` opens on, and the one place the
    /// precedence between "what is being edited" and "what the project settings
    /// say" is decided.
    ///
    /// In order:
    ///
    /// 1. **The active composition's format**, when there is one. A local signal
    ///    beats a project-wide default: someone working in a 24 fps sequence is
    ///    making another shot of it, not a fresh 30 fps composition.
    /// 2. **The project settings' default frame rate**
    ///    ([`app_settings::default_frame_rate`], `SET-6`) over the fallback
    ///    format, when nothing is active.
    /// 3. **[`CompositionSettings::fallback`]** for the rest, and for the frame
    ///    rate too when the setting cannot be read.
    ///
    /// The consequence to know before hunting a bug here: **step 1 hides the
    /// setting.** With a composition open, changing the default frame rate
    /// changes nothing about the next `Composition ▸ New…` — the setting decides
    /// the root composition of `File ▸ New` and the case where the document has
    /// no active composition. That is deliberate (the plan does not ask for the
    /// inheritance to change), not an unwired setting.
    ///
    /// Opening a `.ravprj` is a third thing again and does not come through
    /// here: its compositions are saved facts, and a setting must not rewrite
    /// them.
    pub fn new_composition_defaults(&self, cx: &App) -> CompositionSettings {
        let name = next_composition_name(self.store.document());
        match self.active_composition(cx) {
            Some(active) => CompositionSettings {
                name,
                ..CompositionSettings::from_composition(active)
            },
            None => CompositionSettings {
                frame_rate: app_settings::default_frame_rate(cx),
                ..CompositionSettings::fallback(name)
            },
        }
    }

    /// Create a composition from `settings` and make it the active one. One
    /// undo step: the settings are already final when this is called (the
    /// dialog holds unconfirmed values), so nothing has to be created and then
    /// corrected.
    pub fn create_composition(
        &mut self,
        settings: CompositionSettings,
        cx: &mut Context<Self>,
    ) -> CompId {
        let (doc, id) = add_composition(self.store.document(), settings);
        self.commit_document(doc, InvalidationHint::Structural, cx);
        self.set_active_composition(Some(id), cx);
        id
    }

    /// Replace a composition's settings, keeping its layers. One undo step.
    pub fn apply_composition_settings(
        &mut self,
        comp: CompId,
        settings: CompositionSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = update_composition(self.store.document(), comp, |current| {
            settings.apply_to(current)
        }) else {
            return;
        };
        self.commit_document(doc, InvalidationHint::Structural, cx);
    }

    /// Deep-copy a composition and switch to the copy — the copy is what the
    /// user goes on to edit. One undo step.
    pub fn duplicate_composition(
        &mut self,
        comp: CompId,
        cx: &mut Context<Self>,
    ) -> Option<CompId> {
        let (doc, id) = duplicate_composition(self.store.document(), comp)?;
        self.commit_document(doc, InvalidationHint::Structural, cx);
        self.set_active_composition(Some(id), cx);
        Some(id)
    }

    /// Delete a composition. When it was the active one, the neighbour in
    /// display order takes over (`None` when it was the last composition —
    /// composition 0 is a valid state). One undo step.
    ///
    /// Undo restores the document, but not which composition was active: a
    /// composition switch is UI state and deliberately outside the undo
    /// history, so an undone delete leaves the neighbour active until the user
    /// switches back.
    pub fn delete_composition(&mut self, comp: CompId, cx: &mut Context<Self>) {
        let successor = neighbour_composition(self.store.document(), comp);
        let Some(doc) = remove_composition(self.store.document(), comp) else {
            return;
        };
        self.commit_document(doc, InvalidationHint::Structural, cx);
        crate::panels::drop_composition_properties_target(comp, cx);
        if crate::panels::active_composition(cx) == Some(comp) {
            self.set_active_composition(successor, cx);
        }
    }

    fn document_changed(&mut self, hint: InvalidationHint, cx: &mut Context<Self>) {
        // The compiled chain is topology, not values. Every shell processor
        // resolves its layer from the `Document` the request carries and reads
        // transform, opacity, timing, parenting and the layer network from it
        // at process time, so a parameter edit reaches the viewer through the
        // document without the chain being rebuilt. What the chain *does* bake
        // in is the shape: which layers are active (solo/mute), their order,
        // the parent edges, the blend mode (it picks the merge node's type key)
        // and the adjustment flag (it picks a different merge and drops the
        // opacity node). Every edit that moves one of those passes
        // `Structural` — `apply_layer_change` and `toggle_layer_flag` both
        // spell that list out — so `Structural` is the exact gate here.
        //
        // Dropping it unconditionally made every mouse move of a scrub
        // recompile the active composition on the UI thread, at a cost linear
        // in the layer count (`MED-UI-01`).
        if matches!(hint, InvalidationHint::Structural) {
            self.compiled = None;
        }
        self.mirror_epoch += 1;
        // The band goes now, not when the next evaluation lands: the panel
        // repaints from the notify at the bottom of this function, and a band
        // published before the edit would claim frames the frame cache is
        // about to drop. It comes back frame by frame as evaluations
        // complete.
        self.clear_cache_band(cx);
        // Only a topology change can add or remove nodes; a parameter edit
        // (a scrub drag, one call per mouse move) leaves the node set alone.
        if matches!(hint, InvalidationHint::Structural) {
            self.structure_epoch += 1;
        }
        // Every document change funnels through here (edit, revert, undo,
        // redo), which is the one place that can keep the shared layer
        // selection free of layers the document has lost — no panel has to
        // exist for that to hold.
        let document = self.store.document().clone();
        crate::panels::prune_layer_selection(&document, cx);
        crate::panels::prune_media_selection(&document, cx);
        // Dropping a deleted node's readout here, and not on the next
        // evaluation result, is what keeps it from being inherited by a node
        // that reuses the id later.
        self.prune_eval_timings(cx);
        crate::audio::sync_from_document(self.store.document(), cx);
        self.request_viewer_eval(hint, cx);
        cx.notify();
    }

    // ----- viewer evaluation ---------------------------------------------------

    /// Fraction of the composition resolution the viewer evaluates at
    /// (REQ-UI-004).
    pub fn viewer_resolution(&self) -> ViewerResolution {
        self.viewer_resolution
    }

    /// Choose the preview resolution factor and re-evaluate at it.
    ///
    /// The hint is [`InvalidationHint::None`] on purpose: nothing about the
    /// document changed, and the evaluator's cache identity already carries
    /// the target resolution, so the results computed at the previous factor
    /// are missed rather than served (`CacheMiss::ResolutionChanged`). Marking
    /// nodes dirty here would throw away results the other factors can still
    /// use when the user switches back.
    pub fn set_viewer_resolution(&mut self, resolution: ViewerResolution, cx: &mut Context<Self>) {
        if self.viewer_resolution == resolution {
            return;
        }
        self.viewer_resolution = resolution;
        self.request_viewer_eval(InvalidationHint::None, cx);
        cx.notify();
    }

    /// Post one background evaluation of the active composition output at the
    /// current playback position (REQ-LAYER-007). The worker coalesces
    /// rapid-fire requests latest-wins; hints of skipped requests are merged
    /// there, and hints that could not be posted at all are retained
    /// locally.
    pub fn request_viewer_eval(&mut self, hint: InvalidationHint, cx: &mut Context<Self>) {
        self.report_gpu_device_loss(false, cx);
        // Accumulate first: every early return below must retain the hint.
        let pending = std::mem::replace(&mut self.pending_hint, InvalidationHint::None);
        self.pending_hint = pending.merge(hint);

        let position = cx
            .try_global::<crate::panels::PlaybackPosition>()
            .copied()
            .unwrap_or_default();

        let request =
            match self.build_viewer_request(position.frame, &OverlayRegistry::builtin(), cx) {
                Ok(Some(request)) => request,
                Ok(None) => {
                    // Nothing evaluable (no active composition, or an empty
                    // one): blank the viewer and outdate in-flight results (the
                    // fence keeps an older in-flight result from overwriting the
                    // blank).
                    if let Some(eval) = self.eval.as_mut() {
                        self.published_generation = eval.cancel_pending();
                    }
                    let frame = self.viewer_blank(cx);
                    cx.set_global(frame);
                    // No evaluation follows, so nothing would replace the overlay
                    // snapshot: drop it here or the overlays keep drawing over a
                    // blank viewer.
                    cx.set_global(EvalResults::default());
                    // Nothing will be evaluated, so nothing will republish the
                    // band: it has to be cleared on the way out or an emptied
                    // composition keeps the band of the one before it forever.
                    self.clear_cache_band(cx);
                    return;
                }
                Err(err) => {
                    // The composition no longer compiles: surface the error in
                    // the viewer — a silent blank would read as "empty", not
                    // "broken".
                    tracing::error!(%err, "active composition compilation failed");
                    if let Some(eval) = self.eval.as_mut() {
                        self.published_generation = eval.cancel_pending();
                    }
                    let frame = self.viewer_error(err.to_string().into(), cx);
                    cx.set_global(frame);
                    // Same reason as the blank path above: no evaluation follows
                    // a composition that does not compile.
                    cx.set_global(EvalResults::default());
                    self.clear_cache_band(cx);
                    return;
                }
            };
        let hint = std::mem::replace(&mut self.pending_hint, InvalidationHint::None);
        if let Some(eval) = self.eval.as_mut() {
            eval.request(EvalRequest { hint, ..request });
        } else {
            // No worker (tests): the hint stays pending, and no result will
            // arrive to replace the snapshot.
            self.pending_hint = hint;
            cx.set_global(EvalResults::default());
        }
    }

    /// Assemble the active-composition evaluation request, without the hint
    /// (filled by the caller). `Ok(None)` when nothing is evaluable,
    /// `Err` when the composition fails to compile.
    ///
    /// `overlays` is a parameter rather than a call to
    /// [`OverlayRegistry::builtin`] so a test can supply overlays that
    /// actually declare an [`EvalTarget`] — no built-in one does yet — and
    /// still go through this, the production assembly path.
    fn build_viewer_request(
        &mut self,
        frame: u64,
        overlays: &OverlayRegistry,
        cx: &App,
    ) -> Result<Option<EvalRequest>, CompileError> {
        let document = Arc::new(self.store.document().clone());
        let Some(comp) = crate::panels::active_composition_in(&document, cx).cloned() else {
            return Ok(None);
        };
        let ctx = self.viewer_eval_context(&comp, frame);
        let overlay_context = self.overlay_context_for_request(&document, &comp, frame, cx);
        let Some(compiled) = self.compiled_root(cx)? else {
            return Ok(None);
        };
        let graph = compiled.graph.clone();
        let output = compiled.output;
        // The root scope: this request evaluates the composition's compiled
        // shell chain. Overlay targets name networks *inside* it and travel in
        // `scoped`, each carrying the graph and path it is evaluated under.
        let path = Vec::new();
        let nodes = vec![output];
        let scoped = scoped_eval_targets(&document, &ctx, overlays, &overlay_context);
        Ok(Some(EvalRequest {
            comp: Some(comp.id),
            // The composition output stays target 0: `ViewerUpdate::from_eval`
            // reads that position, and overlay targets ride in `scoped` rather
            // than displacing it.
            graph,
            nodes,
            scoped,
            path,
            ctx,
            document: Some(document),
            hint: InvalidationHint::None,
        }))
    }

    /// The world an overlay is allowed to see while its evaluation targets
    /// are collected.
    ///
    /// Only the fields `is_active` / `eval_target` can legitimately depend on
    /// are filled: theme colors, the panel's grid and safe-area toggles and
    /// the last error are presentation, and this runs with no window and no
    /// installed theme. `results` is deliberately the *current* snapshot, so
    /// an overlay that decides its target from what it already has keeps
    /// asking for the same one.
    fn overlay_context_for_request(
        &self,
        document: &Document,
        comp: &Composition,
        frame: u64,
        cx: &App,
    ) -> OverlayContext {
        OverlayContext {
            resolution: Some(comp.resolution),
            eval_resolution: Some(self.viewer_resolution.apply(comp.resolution)),
            playback: cx
                .try_global::<crate::panels::PlaybackPosition>()
                .copied()
                .or(Some(crate::panels::PlaybackPosition {
                    frame,
                    fps: comp.frame_rate,
                })),
            document: Some(document.clone()),
            selection: cx.try_global::<crate::panels::CanvasSelection>().cloned(),
            layer_selection: crate::panels::layer_selection(cx),
            tool: cx
                .try_global::<crate::panels::ToolState>()
                .map(|state| state.active),
            results: cx.try_global::<EvalResults>().cloned().unwrap_or_default(),
            registry: Some(self.registry.clone()),
            ..OverlayContext::default()
        }
    }

    /// The evaluation context the viewer asks for, at `frame`.
    ///
    /// One place, because the frame cache keys on every axis of it: the band
    /// (`CACHE-6`) has to ask with the *same* context the request carries, or
    /// it reports entries a scrub would miss. The interactive path is also
    /// the one place that opts down the quality axis — the viewer wants a
    /// responsive picture, not the sample count an export pays for. The
    /// preview resolution factor is an independent axis (it scales the
    /// buffer, quality counts samples), so the two combine freely.
    fn viewer_eval_context(&self, comp: &Composition, frame: u64) -> EvalContext {
        EvalContext::new(
            frame,
            comp.frame_rate,
            self.viewer_resolution.apply(comp.resolution),
        )
        .with_comp_resolution(comp.resolution)
        .with_quality(Quality::Preview)
    }

    /// `Ok(None)`: nothing to draw (no active composition, or no active
    /// layers). `Err`: the composition exists but failed to compile — the
    /// caller surfaces this in the viewer instead of blanking it.
    fn compiled_root(&mut self, cx: &App) -> Result<Option<&CompiledRoot>, CompileError> {
        if self.compiled.is_none() {
            let Some(comp) = crate::panels::active_composition_in(self.store.document(), cx) else {
                return Ok(None);
            };
            match compile_composition(comp, Graph::new()) {
                Ok(result) => {
                    self.compiled = Some(CompiledRoot {
                        graph: result.graph,
                        output: result.output_node,
                    });
                }
                Err(CompileError::NoActiveLayers(_)) => return Ok(None),
                Err(err) => return Err(err),
            }
        }
        Ok(self.compiled.as_ref())
    }

    /// Refresh [`Self::live_nodes`] and drop every [`NodeEvalTimings`] entry
    /// the document no longer has.
    ///
    /// Cheap to call: it returns immediately unless the structure moved, so
    /// the document scan and the sweep run once per topology change rather
    /// than once per evaluated frame. The global is only rewritten when
    /// something was actually dropped, so a no-op sweep wakes no observer.
    fn prune_eval_timings(&mut self, cx: &mut Context<Self>) {
        if self.live_nodes_epoch == Some(self.structure_epoch) {
            return;
        }
        self.live_nodes = document_node_ids(self.store.document());
        self.live_nodes_epoch = Some(self.structure_epoch);

        let Some(mut timings) = cx.try_global::<NodeEvalTimings>().cloned() else {
            return;
        };
        let before = timings.0.len();
        timings.0.retain(|id, _| self.live_nodes.contains(id));
        if timings.0.len() != before {
            cx.set_global(timings);
        }
    }

    /// An error state for the viewer, carrying the composition resolution so
    /// the panel can draw its black frame behind the message.
    fn viewer_error(&self, message: gpui::SharedString, cx: &App) -> crate::panels::ViewerFrame {
        let composition_resolution = self.active_composition(cx).map(|c| c.resolution);
        crate::panels::ViewerFrame::Error {
            message,
            composition_resolution,
        }
    }

    fn viewer_blank(&self, cx: &App) -> crate::panels::ViewerFrame {
        crate::panels::ViewerFrame::Blank {
            composition_resolution: self.active_composition(cx).map(|c| c.resolution),
        }
    }

    /// Receives a background evaluation result. Any result newer than the
    /// displayed one is published; older results are dropped (but their
    /// timings still update the load readout). Requiring the very latest
    /// generation instead would starve the viewer under load: with the
    /// generation advancing every playback tick, an evaluation slower than
    /// one frame interval is always "stale" by completion, so nothing would
    /// ever be published while playing. Monotonic acceptance still keeps
    /// ordering — an older in-flight result can never overwrite a newer
    /// one, and direct blanks fence via `published_generation`. A failed
    /// evaluation publishes [`ViewerFrame::Error`] — keeping the previous
    /// frame would show content the document no longer produces (e.g. a
    /// deleted Geometry node still visible because the Rasterize input went
    /// missing).
    ///
    /// Results reach the UI through globals only ([`crate::panels::ViewerFrame`]
    /// and [`NodeEvalTimings`]), never through an entity notify. `ProjectState`
    /// observers are panels that mirror the *document*, and the document does
    /// not change when an evaluation completes: notifying them here made all
    /// five rebuild their models on every playback frame. A panel that needs
    /// evaluation output subscribes to the global that carries it.
    fn on_eval_update(&mut self, update: ViewerUpdate, cx: &mut Context<Self>) {
        // Unconditional: a result carrying no timings must not leave a
        // deleted node's readout behind for the next id to inherit.
        self.prune_eval_timings(cx);
        if !update.timings.is_empty() {
            let mut timings = cx
                .try_global::<NodeEvalTimings>()
                .cloned()
                .unwrap_or_default();
            // The evaluator measures the *compiled* graph, which also
            // contains the synthetic compositing nodes. They are never
            // displayed, so storing them would only grow the global for the
            // lifetime of the session.
            timings.0.extend(
                update
                    .timings
                    .iter()
                    .filter(|(id, _)| self.live_nodes.contains(id))
                    .copied(),
            );
            cx.set_global(timings);
        }

        if update.generation <= self.published_generation {
            // The worker logged this result as sent; pairing that with an
            // explicit drop keeps "worker Ok but nothing published" visible
            // in the log.
            tracing::debug!(
                generation = update.generation,
                published = self.published_generation,
                frame = update.frame,
                dropped = true,
                "viewer update dropped (older than published)"
            );
            return;
        }
        let scoped_results: HashMap<_, _> = update.scoped_results.into_iter().collect();
        // Nothing here touches pixels: the worker already produced the
        // display image (HIGH-08), so publishing is a move.
        let frame = match update.output {
            ViewerOutput::Image(image) => crate::panels::ViewerFrame::Frame {
                composition_resolution: self
                    .active_composition(cx)
                    .map(|c| c.resolution)
                    .unwrap_or((image.width(), image.height())),
                image,
            },
            ViewerOutput::Gpu(frame) => crate::panels::ViewerFrame::GpuFrame {
                composition_resolution: self
                    .active_composition(cx)
                    .map(|c| c.resolution)
                    .unwrap_or((frame.width(), frame.height())),
                frame,
            },
            ViewerOutput::NotAFrame => self.viewer_blank(cx),
            ViewerOutput::Failed(message) => {
                tracing::debug!(%message, "viewer evaluation failed");
                self.viewer_error(message.into(), cx)
            }
        };
        let published = match &frame {
            crate::panels::ViewerFrame::Frame { .. } => "frame",
            crate::panels::ViewerFrame::GpuFrame { .. } => "gpu-frame",
            crate::panels::ViewerFrame::Blank { .. } => "blank",
            crate::panels::ViewerFrame::Error { .. } => "error",
        };
        // Published with the frame this update carries — and only when that
        // frame is an image. An overlay drawn over a blank or an error frame
        // annotates a composition that is not on screen, which is the same
        // mistake as painting a result that has not arrived. Replacing the
        // whole map is what makes a target that failed or was dropped read as
        // absent instead of keeping the value it had two frames ago.
        let scoped_results = match &frame {
            crate::panels::ViewerFrame::Frame { .. }
            | crate::panels::ViewerFrame::GpuFrame { .. } => scoped_results,
            crate::panels::ViewerFrame::Blank { .. } | crate::panels::ViewerFrame::Error { .. } => {
                HashMap::new()
            }
        };
        cx.set_global(EvalResults::new(scoped_results));
        tracing::debug!(
            generation = update.generation,
            frame = update.frame,
            published,
            "viewer frame published"
        );
        self.published_generation = update.generation;
        cx.set_global(frame);
        self.publish_cache_band(cx);
    }

    /// Republish the active composition's cached frame ranges for the
    /// Timeline's cache band (`CACHE-6`).
    ///
    /// Called when an evaluation completes, which is the only moment the
    /// frame cache can have grown, and asked at the resolution and precision
    /// the viewer is *currently* requesting — a band drawn from another
    /// factor's entries would claim frames a scrub would miss.
    ///
    /// [`crate::panels::set_cache_band`] compares before it writes, so an
    /// evaluation that added nothing (a cache hit, a failed target) does not
    /// touch the global at all. Nothing observes it either: the Timeline
    /// reads it while repainting for the playhead or the document, which is
    /// what keeps the band off the repaint budget (`HIGH-21`).
    fn publish_cache_band(&mut self, cx: &mut Context<Self>) {
        let Some(eval) = self.eval.as_ref() else {
            return;
        };
        // `cached_ranges` walks every cached frame and sorts. An evaluation
        // served from the cache added none, so the version guard turns the
        // scan into one atomic read for exactly the requests a user makes
        // fastest — scrubbing back over frames already visited.
        let version = eval.frame_cache().version();
        if self.published_band_version == Some(version) {
            return;
        }
        let Some(comp) = self.active_composition(cx) else {
            return;
        };
        let id = comp.id;
        // The very context the next request will carry, so the band and the
        // hit test agree on every axis.
        let ranges = eval
            .frame_cache()
            .cached_ranges(id, &self.viewer_eval_context(comp, 0));
        self.published_band_version = Some(version);
        crate::panels::set_cache_band(id, ranges, cx);
    }

    /// Drop the Timeline's cache band and the version it was computed at, so
    /// the next evaluation republishes it even if the frame cache did not
    /// change in between (an edit to another composition, say).
    fn clear_cache_band(&mut self, cx: &mut App) {
        self.published_band_version = None;
        crate::panels::clear_cache_band(cx);
    }

    /// Frame rate and duration of the active composition, for the playback
    /// clock.
    pub fn playback_params(&self, cx: &App) -> Option<(FrameRate, u64)> {
        self.active_composition(cx)
            .map(|c| (c.frame_rate, c.duration_frames))
    }
}

impl EventEmitter<ProjectEvent> for ProjectState {}
impl EventEmitter<DocumentReplaced> for ProjectState {}

/// The active overlays' evaluation targets, each resolved to the graph and the
/// ownership path it must be evaluated under.
///
/// Every [`NetworkPath`] names a composition *and* a layer, so no overlay
/// target ever lives in the composition's compiled shell graph: a layer
/// network is evaluated recursively through its boundary node rather than
/// inlined. Each target therefore carries its own scope
/// ([`ravel_core::runtime::ScopedTarget`]) and the worker pulls it with
/// [`ravel_core::eval::Evaluator::evaluate_at`], through the same evaluator
/// that just ran the shell — so a node the composition already composited is a
/// cache hit rather than a second pull.
///
/// Membership in the request's graph would have been the wrong test and is not
/// used anywhere: a hit could only ever be an id collision, because
/// `deterministic_node_id` (`comp << 32 | layer << 8 | role`) lands in the
/// ordinary node-id range whenever the composition id is 0, and the overlay
/// would then be painted from an unrelated compositing node.
///
/// Two folds happen here. [`OverlayRegistry::eval_targets`] has already
/// dropped exact duplicates; this drops targets that differ only in output
/// port, because evaluation is per node. A target whose network or node no
/// longer resolves is dropped rather than requested — the overlay then reads
/// no result and draws nothing.
fn scoped_eval_targets(
    document: &Document,
    ctx: &EvalContext,
    overlays: &OverlayRegistry,
    context: &OverlayContext,
) -> Vec<ScopedTarget> {
    let mut scoped: Vec<ScopedTarget> = Vec::new();
    for target in overlays.eval_targets(context) {
        let path = target.network.segments();
        if scoped
            .iter()
            .any(|existing| existing.node == target.node && existing.path == path)
        {
            continue;
        }
        let Some(graph) = ravel_ui::document::resolve_network(document, &target.network) else {
            continue;
        };
        if graph.node(target.node).is_none() {
            continue;
        }
        let Some(layer) = document
            .get_composition(target.network.comp)
            .and_then(|comp| comp.get_layer(target.network.layer))
        else {
            continue;
        };
        // Layer-local time (REQ-LAYER-006) and the shell's own interval check
        // in one answer: outside `[in, out)` the compositing chain does not
        // evaluate the network at all, so asking for a node inside it would
        // both draw an overlay over a layer that is not on screen and pay for
        // an evaluation the frame did not need.
        //
        // `Layer::local_frame` is the wrong question here: it clamps at zero,
        // so a layer that starts at composition frame 5 reports local frame 0
        // — its own `in_frame` — at composition frame 0 and reads as showing.
        let Some(local_frame) = layer.displayed_local_frame(ctx.frame) else {
            continue;
        };
        scoped.push(ScopedTarget {
            path,
            graph: graph.clone(),
            node: target.node,
            // The very context `comp.network` enters the layer network with,
            // so this pull lands on the cache entry the shell evaluation just
            // filled instead of computing it a second time.
            ctx: ctx.with_frame(local_frame),
        });
    }
    scoped
}

/// A readable, collision-free asset id derived from the file name.
fn unique_asset_id(doc: &Document, path: &Path) -> String {
    let base = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "asset".to_string());
    if !doc.media_assets.contains_key(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base} {n}");
        if !doc.media_assets.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::viewer::overlay::{EvalTarget, OverlayId, ViewerOverlay};
    use gpui::{AppContext as _, TestAppContext};
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::composition::{BlendMode, Layer};
    use ravel_core::eval::{PathSegment, ProcessorRegistry as _};
    use ravel_core::graph::{Node, ParameterValue};
    use ravel_core::id::{DataTypeId, LayerId, OutputPortIndex};
    use ravel_core::network as net;
    use ravel_core::types::FrameBuffer;
    use ravel_ui::document::NetworkPath;

    struct TargetOverlay {
        target: EvalTarget,
    }

    impl ViewerOverlay for TargetOverlay {
        fn id(&self) -> OverlayId {
            OverlayId("test.request-target")
        }

        fn priority(&self) -> i32 {
            0
        }

        fn is_active(&self, _ctx: &OverlayContext) -> bool {
            true
        }

        fn eval_targets(&self, _ctx: &OverlayContext) -> Vec<EvalTarget> {
            vec![self.target.clone()]
        }
    }

    /// The default is what Ravel has always launched with — one composition,
    /// which is also the root — and turning the setting off launches with none
    /// rather than with an empty one.
    #[gpui::test]
    fn a_fresh_document_follows_the_startup_composition_setting(cx: &mut TestAppContext) {
        cx.update(|cx| {
            app_settings::install(app_settings::GlobalSettingsFile::default(), cx);
            let default = fresh_document(cx);
            assert_eq!(default.compositions.len(), 1);
            assert!(default.root_comp.is_some());

            app_settings::update(
                app_settings::SettingsScope::Global,
                |layer| layer.startup.create_composition = Some(false),
                cx,
            );
            let empty = fresh_document(cx);
            assert!(empty.compositions.is_empty());
            assert_eq!(empty.root_comp, None);
        });
    }

    #[derive(Default)]
    struct ProjectEventRecorder(Vec<ProjectEvent>);

    fn record_events(
        project: &gpui::Entity<ProjectState>,
        cx: &mut TestAppContext,
    ) -> gpui::Entity<ProjectEventRecorder> {
        let recorder = cx.new(|_| ProjectEventRecorder::default());
        recorder.update(cx, |_, cx| {
            cx.subscribe(project, |recorder, _project, event: &ProjectEvent, _cx| {
                recorder.0.push(event.clone());
            })
            .detach();
        });
        recorder
    }

    /// A frame as the worker now delivers it: display bytes, not linear
    /// light (`CM-7`). The colour is irrelevant to these tests — what matters
    /// is that a viewer result is a `DisplayFrame`.
    fn blank_display_frame(width: u32, height: u32) -> Arc<dyn ravel_core::types::NodeData> {
        Arc::new(ravel_nodes::DisplayFrame::new(
            width,
            height,
            Arc::from(vec![0u8; (width as usize) * (height as usize) * 4]),
        ))
    }

    /// Emits a fixed frame, so an evaluation produces something the viewer
    /// path has to convert.
    struct FrameSource;

    impl ravel_core::eval::NodeProcessor for FrameSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn ravel_core::types::NodeData>>],
            _params: &ravel_core::eval::ResolvedParams,
            _scope: &mut dyn ravel_core::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn ravel_core::types::NodeData>> {
            Ok(Arc::new(FrameBuffer::from_f32(
                2,
                2,
                [1.0, 0.5, 0.0, 1.0].repeat(4),
            )))
        }
    }

    /// Hooks that need no GPU: every node emits a frame, and `finalize`
    /// stands in for what `GpuEvalHooks` does on the GPU — hand the viewer
    /// display bytes rather than linear light.
    struct FrameHooks;

    impl EvalWorkerHooks for FrameHooks {
        fn finalize(
            &mut self,
            value: &Arc<dyn ravel_core::types::NodeData>,
            _ctx: &EvalContext,
        ) -> Option<Arc<dyn ravel_core::types::NodeData>> {
            let Some(fb) = value.downcast_ref::<FrameBuffer>() else {
                return Some(value.clone());
            };
            let mut bgra = Vec::with_capacity(fb.as_f32().len());
            for pixel in fb.as_f32().chunks_exact(4) {
                let display =
                    ravel_core::color::to_display_rgba8([pixel[0], pixel[1], pixel[2], pixel[3]]);
                bgra.extend_from_slice(&[display[2], display[1], display[0], display[3]]);
            }
            Some(Arc::new(ravel_nodes::DisplayFrame::new(
                fb.width,
                fb.height,
                Arc::from(bgra),
            )))
        }

        fn sync(
            &mut self,
            evaluator: &mut ravel_core::runtime::ProcessorSync<'_>,
            graph: &Graph,
            _document: Option<&Document>,
            _hint: &InvalidationHint,
        ) {
            for node in graph.nodes() {
                evaluator.register(node.id, Arc::new(FrameSource));
            }
        }
    }

    /// HIGH-08: the f32→BGRA conversion must not run on the UI thread. The
    /// wiring under test is the production one — `spawn_viewer_eval_service`
    /// is what `ProjectState::new` calls — so what this pins is *where* the
    /// conversion happens, not merely that a helper can be called off-thread.
    #[test]
    fn the_display_conversion_runs_on_the_evaluation_worker() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ViewerUpdate>();
        let budget = SharedCacheBudget::new(
            ravel_project::settings::ResolvedSettings::default().cache_budget(),
        );
        let mut service = spawn_viewer_eval_service(FrameHooks, budget, tx);

        let node = NodeId::new(1);
        let graph = Graph::new()
            .add_node(Node::new(node, "test.frame").with_output("out", DataTypeId::FRAME_BUFFER))
            .expect("graph");
        service.request(EvalRequest {
            comp: None,
            graph,
            nodes: vec![node],
            scoped: Vec::new(),
            path: Vec::new(),
            ctx: EvalContext::new(0, FrameRate::new(30, 1), (2, 2)),
            document: None,
            hint: InvalidationHint::Structural,
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let update = loop {
            if let Ok(update) = rx.try_recv() {
                break update;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the evaluation worker sent no result"
            );
            std::thread::yield_now();
        };

        let ViewerOutput::Image(image) = &update.output else {
            panic!("expected a converted frame");
        };
        // The conversion really ran: BGRA of the working-space pixel
        // (1.0, 0.5, 0.0, 1.0) displayed through sRGB. Green was 128 before
        // `CM-3` inserted the display transform; 0.5 linear is 188.
        assert_eq!(
            &image.image().as_bytes(0).expect("frame 0")[..4],
            &[0, 188, 255, 255]
        );
        // ... and it ran on the worker, not on this (UI-role) thread.
        let converted_on = image.converted_on();
        assert_ne!(
            converted_on.id,
            std::thread::current().id(),
            "the conversion ran on the thread that publishes to the UI"
        );
        assert_eq!(
            converted_on.name.as_deref(),
            Some("ravel-eval-service"),
            "the conversion must run on the evaluation worker"
        );
    }

    /// The paths where **no evaluation follows** — an emptied composition, one
    /// that stopped compiling — have to clear the band on their way out *and*
    /// forget the frame-cache version it was published at.
    ///
    /// Forgetting the version is the half that is easy to miss: without it,
    /// returning to a fully cached composition is all cache hits, the version
    /// never moves, and `publish_cache_band`'s recompute guard skips forever —
    /// the band would be gone for the rest of the session (`CACHE-6`).
    #[gpui::test]
    fn a_path_with_no_evaluation_clears_the_band_and_its_version(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let comp_id = project.read_with(cx, |project, _| {
            project.document().root_comp.expect("root comp")
        });
        cx.update(|cx| {
            crate::panels::set_active_composition(Some(comp_id), cx);
            crate::panels::set_cache_band(comp_id, vec![0..10, 20..30], cx);
        });
        project.update(cx, |project, _cx| {
            project.published_band_version = Some(7);
        });

        // No active composition: `build_viewer_request` returns `Ok(None)`,
        // the viewer is blanked, and nothing is ever evaluated.
        cx.update(|cx| crate::panels::set_active_composition(None, cx));
        project.update(cx, |project, cx| {
            project.request_viewer_eval(InvalidationHint::None, cx);
        });

        // Read the band back through the composition it belonged to.
        cx.update(|cx| {
            crate::panels::set_active_composition(Some(comp_id), cx);
            assert!(
                crate::panels::cache_band(cx).is_empty(),
                "the blank path kept the band of the composition before it"
            );
        });
        project.read_with(cx, |project, _| {
            assert_eq!(
                project.published_band_version, None,
                "the band was cleared but its version was latched: it can \
                 never be republished from a cache that stops changing"
            );
        });
    }

    #[gpui::test]
    fn save_and_open_failures_emit_visible_operation_events(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let recorder = record_events(&project, cx);
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"file").unwrap();
        let save_path = blocker.join("project.ravprj");

        project.update(cx, |project, cx| {
            project.save_project_to(save_path.clone(), None, cx);
        });
        cx.run_until_parked();
        assert!(recorder.read_with(cx, |recorder, _| recorder.0.iter().any(
            |event| matches!(event, ProjectEvent::SaveFailed { path, .. } if path == &save_path)
        )));

        let missing = dir.path().join("missing.ravprj");
        project.update(cx, |project, cx| {
            project.load_project_from(missing.clone(), cx);
        });
        cx.run_until_parked();
        assert!(recorder.read_with(cx, |recorder, _| recorder.0.iter().any(
            |event| matches!(event, ProjectEvent::OpenFailed { path, too_new: false, .. } if path == &missing)
        )));

        let skipped = crate::media::import::ImportFailure {
            path: dir.path().join("unsupported.xyz"),
            reason: "unsupported format".into(),
        };
        project.update(cx, |project, cx| {
            project.import_media(Vec::new(), vec![skipped.clone()], cx);
        });
        assert!(
            recorder.read_with(cx, |recorder, _| recorder.0.iter().any(|event| {
                matches!(
                    event,
                ProjectEvent::MediaImportSkipped { failures }
                    if failures == std::slice::from_ref(&skipped)
                )
            }))
        );
    }

    #[gpui::test]
    fn gpu_device_loss_emits_one_session_event(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let recorder = record_events(&project, cx);

        project.update(cx, |project, cx| {
            // Every paint that still holds the dead frame reports again; only
            // the first one may reach the user.
            project.report_gpu_device_loss(true, cx);
            project.report_gpu_device_loss(true, cx);
            project.configure_viewer_surface(false, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            recorder.read_with(cx, |recorder, _| {
                recorder
                    .0
                    .iter()
                    .filter(|event| matches!(event, ProjectEvent::GpuDeviceLost))
                    .count()
            }),
            1,
            "a device loss is announced once per session"
        );
    }

    #[gpui::test]
    fn superseded_open_failure_does_not_emit_a_stale_error(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let recorder = record_events(&project, cx);
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.ravprj");
        let valid = dir.path().join("valid.ravprj");
        let document = project.read_with(cx, |project, _| project.document().clone());
        ravel_project::ProjectFile::from_document("valid", "2026-01-01T00:00:00Z", document)
            .save(&valid)
            .unwrap();

        project.update(cx, |project, cx| {
            project.load_project_from(missing.clone(), cx);
            project.load_project_from(valid.clone(), cx);
        });
        cx.run_until_parked();

        assert_eq!(
            project.read_with(cx, |project, _| project
                .project_path()
                .map(Path::to_path_buf)),
            Some(valid)
        );
        assert!(recorder.read_with(cx, |recorder, _| recorder.0.iter().all(
            |event| !matches!(event, ProjectEvent::OpenFailed { path, .. } if path == &missing)
        )));
    }

    #[gpui::test]
    fn viewer_request_keeps_composition_coordinate_resolution(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let (comp_resolution, ctx) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.unwrap();
            let document =
                ravel_ui::document::add_layer(project.document(), comp_id, content_layer())
                    .unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
            let comp_resolution = project.active_composition(cx).unwrap().resolution;
            let request = project
                .build_viewer_request(0, &OverlayRegistry::builtin(), cx)
                .unwrap()
                .unwrap();
            (comp_resolution, request.ctx)
        });

        assert_eq!(
            ctx.resolution,
            ViewerResolution::default().apply(comp_resolution)
        );
        assert_eq!(ctx.comp_resolution, comp_resolution);
    }

    /// A composition with one content layer, plus its compiled shell chain's
    /// output node and one other synthetic node of that graph.
    fn project_with_a_shell_target(
        project: &mut ProjectState,
        cx: &mut Context<ProjectState>,
    ) -> (Composition, NodeId, NodeId) {
        let comp_id = project.document().root_comp.unwrap();
        let document =
            ravel_ui::document::add_layer(project.document(), comp_id, content_layer()).unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
        let comp = project.active_composition(cx).unwrap().clone();
        let compiled = project.compiled_root(cx).unwrap().unwrap();
        let synthetic = compiled
            .graph
            .nodes()
            .find(|node| node.id != compiled.output)
            .expect("the compiled shell chain has more than its output node")
            .id;
        (comp, compiled.output, synthetic)
    }

    /// A scoped outcome as the worker tags one, in an arbitrary but fixed
    /// layer scope — these tests are about publication, not about which
    /// network a value came from.
    fn scoped_result(
        node: NodeId,
        output: ravel_core::runtime::EvalOutput,
    ) -> ravel_core::runtime::ScopedResult {
        ravel_core::runtime::ScopedResult {
            path: vec![PathSegment::Layer(CompId::new(1), LayerId::new(1))],
            node,
            output,
        }
    }

    fn target_in(network: &NetworkPath, node: NodeId) -> EvalTarget {
        EvalTarget {
            network: network.clone(),
            node,
            output: OutputPortIndex(0),
        }
    }

    /// Completion criterion: no active overlay declares a target, so the
    /// request carries the composition output and nothing else.
    #[gpui::test]
    fn viewer_request_without_active_overlay_targets_keeps_only_the_composition_output(
        cx: &mut TestAppContext,
    ) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let (_, output, _) = project_with_a_shell_target(project, cx);
            // The production registry: none of its overlays declares a target.
            let request = project
                .build_viewer_request(0, &OverlayRegistry::builtin(), cx)
                .unwrap()
                .unwrap();
            assert_eq!(request.nodes, vec![output]);
        });
    }

    /// The composition request runs at the root scope and no `NetworkPath`
    /// denotes that scope, so **nothing** rides along on it — not even a
    /// target whose `NodeId` happens to name a node of the compiled shell
    /// graph.
    ///
    /// That collision is the whole point. `deterministic_node_id` packs
    /// `comp << 32 | layer << 8 | role`, so with composition id 0 a synthetic
    /// node lands in the ordinary node-id range; a membership test would
    /// accept this target and hand the overlay a compositing node's result.
    #[gpui::test]
    fn the_composition_request_carries_no_overlay_target_even_on_an_id_collision(
        cx: &mut TestAppContext,
    ) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let (comp, output, synthetic) = project_with_a_shell_target(project, cx);
            let layer = comp.layers.front().unwrap().id;
            // A target naming a node that *is* in the request's graph.
            let network = NetworkPath::layer(comp.id, layer);
            let overlays = OverlayRegistry::new(vec![Box::new(TargetOverlay {
                target: target_in(&network, synthetic),
            })]);

            let request = project
                .build_viewer_request(0, &overlays, cx)
                .unwrap()
                .unwrap();

            assert!(
                request.graph.node(synthetic).is_some(),
                "the collision this test exists for did not occur",
            );
            assert_eq!(
                request.nodes,
                vec![output],
                "an overlay target displaced the composition output",
            );
            // The colliding id is not in the layer network, so nothing is
            // requested for it — and had it been, it would have been pulled
            // from that network's graph, never from the shell graph the
            // collision lives in.
            assert!(request.scoped.is_empty());
        });
    }

    /// The whole point of unit 3: an overlay target reaches the request the
    /// viewer actually posts, carrying its own network's graph and path.
    /// Unit 2's aggregation was dormant precisely because nothing did this.
    #[gpui::test]
    fn the_viewer_request_carries_the_overlays_scoped_targets(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let (comp, output, _) = project_with_a_shell_target(project, cx);
            let network = NetworkPath::layer(comp.id, comp.layers.front().unwrap().id);
            let node = ravel_ui::document::resolve_network(project.document(), &network)
                .expect("the seeded layer network")
                .nodes()
                .next()
                .expect("a node to target")
                .id;
            let overlays = OverlayRegistry::new(vec![Box::new(TargetOverlay {
                target: target_in(&network, node),
            })]);

            let request = project
                .build_viewer_request(0, &overlays, cx)
                .unwrap()
                .unwrap();

            assert_eq!(request.nodes, vec![output], "target 0 is the comp output");
            assert_eq!(request.scoped.len(), 1, "the overlay target was dropped");
            assert_eq!(request.scoped[0].node, node);
            assert_eq!(request.scoped[0].path, network.segments());
            assert!(request.scoped[0].graph.node(node).is_some());
            // Layer-local time, which is the context `comp.network` enters the
            // network with — a request-frame pull would miss that cache entry.
            let layer = comp.layers.front().unwrap();
            assert_eq!(
                request.scoped[0].ctx.frame,
                ravel_ui::keyframes::layer_local_frame(layer, 0),
            );
        });
    }

    /// A layer that has not started yet composites as transparent, so an
    /// overlay must not annotate it — and the evaluation it would need must
    /// not be paid for either.
    ///
    /// The trap this pins: `Layer::local_frame` clamps at zero, so a layer
    /// starting at composition frame 5 reports its own `in_frame` at
    /// composition frame 0 and passes an `>= in_frame` interval test while
    /// showing nothing.
    #[gpui::test]
    fn a_layer_that_has_not_started_contributes_no_scoped_target(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.unwrap();
            let layer = Layer {
                ..content_layer().with_time(5, 0, 300)
            };
            let layer_id = layer.id;
            let document =
                ravel_ui::document::add_layer(project.document(), comp_id, layer).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
            let comp = project.active_composition(cx).unwrap().clone();

            let network = NetworkPath::layer(comp_id, layer_id);
            let node = ravel_ui::document::resolve_network(project.document(), &network)
                .expect("the layer network")
                .nodes()
                .next()
                .expect("a node to target")
                .id;
            let overlays = OverlayRegistry::new(vec![Box::new(TargetOverlay {
                target: target_in(&network, node),
            })]);
            let document = project.document().clone();
            let ctx = project.overlay_context_for_request(&document, &comp, 0, cx);

            let before_start = scoped_eval_targets(
                &document,
                &project.viewer_eval_context(&comp, 0),
                &overlays,
                &ctx,
            );
            assert!(
                before_start.is_empty(),
                "an overlay target rode along for a layer that is not on screen"
            );

            // The very first frame the layer *is* on screen still asks, at the
            // layer-local frame the compositing chain enters the network with.
            let at_start = scoped_eval_targets(
                &document,
                &project.viewer_eval_context(&comp, 5),
                &overlays,
                &ctx,
            );
            assert_eq!(at_start.len(), 1);
            assert_eq!(at_start[0].ctx.frame, 0);
        });
    }

    /// Completion criterion: two overlays wanting the same node cost one
    /// scoped target, hence one evaluation. Folding is by `(scope, node)` —
    /// evaluation is per node, so two targets differing only in output port
    /// are the same pull.
    #[gpui::test]
    fn duplicate_overlay_targets_are_folded_in_the_eval_request(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let (comp, _, _) = project_with_a_shell_target(project, cx);
            let network = NetworkPath::layer(comp.id, comp.layers.front().unwrap().id);
            let node = ravel_ui::document::resolve_network(project.document(), &network)
                .expect("the seeded layer network")
                .nodes()
                .next()
                .expect("a node to target")
                .id;
            let target = target_in(&network, node);
            let mut other_port = target.clone();
            other_port.output = OutputPortIndex(1);
            let overlays = OverlayRegistry::new(vec![
                Box::new(TargetOverlay {
                    target: target.clone(),
                }),
                Box::new(TargetOverlay {
                    target: target.clone(),
                }),
                Box::new(TargetOverlay { target: other_port }),
            ]);
            let document = project.document().clone();
            let ctx = project.overlay_context_for_request(&document, &comp, 0, cx);
            let eval = project.viewer_eval_context(&comp, 0);

            let scoped = scoped_eval_targets(&document, &eval, &overlays, &ctx);

            assert_eq!(scoped.len(), 1, "the same node was pulled more than once");
            assert_eq!(scoped[0].node, node);
            assert_eq!(scoped[0].path, network.segments());
        });
    }

    /// A `NodeId` means nothing without the network it came from. Each target
    /// is therefore evaluated **in its own network**, with that network's
    /// graph and path — never against the request's graph, where an id can
    /// only collide.
    #[gpui::test]
    fn overlay_targets_carry_the_network_they_name(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let (comp, _, synthetic) = project_with_a_shell_target(project, cx);
            let layer = comp.layers.front().unwrap().id;
            let network = NetworkPath::layer(comp.id, layer);
            let node = ravel_ui::document::resolve_network(project.document(), &network)
                .expect("the seeded layer network")
                .nodes()
                .next()
                .expect("a node to target")
                .id;
            let overlays = OverlayRegistry::new(vec![
                Box::new(TargetOverlay {
                    target: target_in(&network, node),
                }),
                // A network that does not exist has nothing to evaluate.
                Box::new(TargetOverlay {
                    target: target_in(
                        &NetworkPath::layer(CompId::new(comp.id.raw() + 1), layer),
                        node,
                    ),
                }),
            ]);
            let document = project.document().clone();
            let ctx = project.overlay_context_for_request(&document, &comp, 0, cx);
            let eval = project.viewer_eval_context(&comp, 0);

            let scoped = scoped_eval_targets(&document, &eval, &overlays, &ctx);

            assert_eq!(scoped.len(), 1, "an unresolvable network was requested");
            assert_eq!(scoped[0].path, network.segments());
            assert!(
                scoped[0].graph.node(node).is_some(),
                "the target was not paired with its own network's graph",
            );
            assert!(
                scoped[0].graph.node(synthetic).is_none(),
                "the shell graph was handed to a layer-network target",
            );
        });
    }

    /// The viewer is the interactive path, so it asks for `Preview` — and it
    /// asks at every preview factor, because the two are independent axes.
    /// `EvalContext` defaults to `Final`, so a viewer request that lost this
    /// call would keep working and just quietly pay export-grade cost, which
    /// is exactly the kind of regression only an assertion catches.
    #[gpui::test]
    fn viewer_request_asks_for_preview_quality(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.unwrap();
            let document =
                ravel_ui::document::add_layer(project.document(), comp_id, content_layer())
                    .unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);

            for factor in ViewerResolution::ALL {
                project.set_viewer_resolution(factor, cx);
                let ctx = project
                    .build_viewer_request(0, &OverlayRegistry::builtin(), cx)
                    .unwrap()
                    .unwrap()
                    .ctx;
                assert_eq!(ctx.quality, Quality::Preview, "{factor:?}");
            }
        });
    }

    /// The preview factor is what decides the evaluation resolution, and the
    /// composition resolution stays the coordinate basis at every factor —
    /// that pair is what keeps the viewer's on-screen scale correct when the
    /// evaluation buffer is smaller than (or equal to) the composition.
    #[gpui::test]
    fn viewer_request_resolution_follows_the_preview_factor(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.unwrap();
            let document =
                ravel_ui::document::add_layer(project.document(), comp_id, content_layer())
                    .unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);

            // The session starts on the factor that preserves the previous
            // interactive cost.
            assert_eq!(project.viewer_resolution(), ViewerResolution::Half);
            let comp_resolution = project.active_composition(cx).unwrap().resolution;

            for factor in ViewerResolution::ALL {
                project.set_viewer_resolution(factor, cx);
                assert_eq!(project.viewer_resolution(), factor);

                let ctx = project
                    .build_viewer_request(0, &OverlayRegistry::builtin(), cx)
                    .unwrap()
                    .unwrap()
                    .ctx;
                assert_eq!(ctx.resolution, factor.apply(comp_resolution), "{factor:?}");
                assert_eq!(ctx.comp_resolution, comp_resolution, "{factor:?}");
            }

            // `Full` evaluates the composition itself: the hidden 1024 px
            // long-edge cap this replaced made that unreachable for any
            // composition larger than the cap.
            project.set_viewer_resolution(ViewerResolution::Full, cx);
            let ctx = project
                .build_viewer_request(0, &OverlayRegistry::builtin(), cx)
                .unwrap()
                .unwrap()
                .ctx;
            assert_eq!(ctx.resolution, comp_resolution);
        });
    }

    /// A layer whose network carries a keyframed custom parameter on the In
    /// node, plus keyframed opacity on the shell.
    fn content_layer() -> Layer {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(24, 1.0, Interpolation::Linear);
        let intensity = AnimationChannel::keyframes(curve);

        let mut opacity_curve = KeyframeCurve::new();
        opacity_curve.insert(0, 0.0, Interpolation::Linear);
        opacity_curve.insert(15, 1.0, Interpolation::Linear);

        let network = Graph::new()
            .add_node(
                Node::new(NodeId::next(), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
                    .with_output(net::PORT_TIME, DataTypeId::SCALAR)
                    .with_output("intensity", DataTypeId::SCALAR)
                    // Current-format In nodes carry `f` (see the load-time
                    // port normalization) so the roundtrip stays exact.
                    .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR)
                    .with_param("intensity", ParameterValue::Channel(intensity)),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::next(), net::NET_OUT_TYPE_KEY)
                    .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap();

        Layer {
            opacity: AnimationChannel::keyframes(opacity_curve),
            ..Layer::new(LayerId::next(), "Solid 1", network)
                .with_time(0, 0, 300)
                .with_blend_mode(BlendMode::Screen)
        }
    }

    /// `MED-UI-01`: dropping the compiled shell chain on every document change
    /// made a scrub recompile the active composition on the UI thread once per
    /// mouse move, at a cost linear in the layer count. The chain is topology;
    /// values live in the `Document` the request carries, and every shell
    /// processor reads them from there at process time.
    ///
    /// Both halves matter, so both are asserted: the chain survives a value
    /// edit (`None` for a layer shell field, `Params` for a node parameter —
    /// the two hints the scrub paths actually send), *and* the edited value is
    /// in the document the next request carries. A test that only checked
    /// retention would pass just as well if the edit stopped reaching the
    /// viewer entirely.
    #[gpui::test]
    fn a_value_edit_keeps_the_compiled_chain_and_still_reaches_the_viewer(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.unwrap();
            let layer = content_layer();
            let layer_id = layer.id;
            let node = layer
                .network
                .nodes()
                .find(|node| node.type_key == net::NET_IN_TYPE_KEY)
                .expect("the content layer has an In node")
                .id;
            let document =
                ravel_ui::document::add_layer(project.document(), comp_id, layer).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);

            project
                .build_viewer_request(0, &OverlayRegistry::builtin(), cx)
                .unwrap()
                .unwrap();
            // The synthetic nodes are `Arc`ed, and a recompile allocates new
            // ones. Pointer identity is therefore the one thing that tells
            // "kept" from "rebuilt to something equal" — the ids are
            // deterministic, so structural equality cannot.
            let merge_id = project
                .compiled
                .as_ref()
                .expect("building a request should have compiled the chain")
                .graph
                .nodes()
                .find(|node| node.type_key.starts_with("comp.merge."))
                .expect("the compiled chain merges the layer over the background")
                .id;
            let compiled_merge = |project: &ProjectState| {
                project
                    .compiled
                    .as_ref()
                    .expect("the chain is compiled")
                    .graph
                    .node(merge_id)
                    .expect("the layer's merge node")
                    .clone()
            };
            let before = compiled_merge(project);

            // A layer shell scrub (`apply_layer_change` sends `None` for every
            // field that is not one of the merge-chain flags).
            let document =
                ravel_ui::document::update_layer(project.document(), comp_id, layer_id, |layer| {
                    layer.opacity = AnimationChannel::constant(0.25);
                })
                .unwrap();
            project.apply_document(document, InvalidationHint::None, cx);
            assert!(
                Arc::ptr_eq(&before, &compiled_merge(project)),
                "a layer value edit must not discard the compiled chain"
            );

            // A node parameter scrub inside the layer network.
            let path = ravel_ui::document::NetworkPath::layer(comp_id, layer_id);
            let network = ravel_ui::document::resolve_network(project.document(), &path)
                .unwrap()
                .clone()
                .set_params(
                    node,
                    &[ravel_core::graph::Parameter {
                        key: "intensity".into(),
                        value: ParameterValue::Channel(AnimationChannel::constant(0.5)),
                    }],
                )
                .unwrap();
            let document =
                ravel_ui::document::replace_network(project.document(), &path, network).unwrap();
            project.apply_document(document, InvalidationHint::Params(vec![node]), cx);
            assert!(
                Arc::ptr_eq(&before, &compiled_merge(project)),
                "a node parameter edit must not discard the compiled chain"
            );

            // The retained chain does not strand the edits: the request carries
            // the live document, which is where the shell processors read.
            let request = project
                .build_viewer_request(0, &OverlayRegistry::builtin(), cx)
                .unwrap()
                .unwrap();
            let document = request.document.as_ref().expect("the request carries one");
            let ctx = EvalContext::new(0, FrameRate::new(30, 1), (16, 16));
            let layer = document
                .get_composition(comp_id)
                .unwrap()
                .get_layer(layer_id)
                .unwrap();
            assert_eq!(
                layer.opacity.evaluate(0.0, &ctx),
                0.25,
                "the layer edit must be in the document the request carries"
            );
            let intensity = layer
                .network
                .node(node)
                .unwrap()
                .parameters
                .iter()
                .find(|param| param.key == "intensity")
                .expect("the In node keeps its intensity parameter");
            assert!(
                matches!(
                    &intensity.value,
                    ParameterValue::Channel(channel) if channel.evaluate(0.0, &ctx) == 0.5
                ),
                "the node edit must be in the document the request carries"
            );

            // A structural edit still rebuilds, and it has to: the blend mode
            // picks the merge node's type key, so it is one of the few values
            // the chain bakes in rather than reads back from the document.
            assert_eq!(before.type_key, "comp.merge.screen");
            let document =
                ravel_ui::document::update_layer(project.document(), comp_id, layer_id, |layer| {
                    layer.blend_mode = BlendMode::Multiply;
                })
                .unwrap();
            project.apply_document(document, InvalidationHint::Structural, cx);
            let after = compiled_merge(project);
            assert!(
                !Arc::ptr_eq(&before, &after),
                "a structural edit must rebuild the compiled chain"
            );
            assert_eq!(
                after.type_key, "comp.merge.multiply",
                "the rebuilt chain must carry the new blend mode"
            );
        });
    }

    /// Stand-in for one of the five panels that observe `ProjectState` to
    /// mirror the document, counting how often it would rebuild its model.
    struct ObserverProbe {
        rebuilds: usize,
        _sub: gpui::Subscription,
    }

    fn observer_probe(
        project: &gpui::Entity<ProjectState>,
        cx: &mut TestAppContext,
    ) -> gpui::Entity<ObserverProbe> {
        let project = project.clone();
        cx.new(|cx| ObserverProbe {
            rebuilds: 0,
            _sub: cx.observe(&project, |this: &mut ObserverProbe, _project, _cx| {
                this.rebuilds += 1;
            }),
        })
    }

    /// RESP-1 regression (CRIT-01): an evaluation result reaches the UI through
    /// globals only. Notifying `ProjectState` observers here made all five
    /// document-mirroring panels rebuild their models on every playback frame,
    /// which multiplied every other render cost by the frame rate.
    #[gpui::test]
    fn eval_results_do_not_rebuild_document_panels(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let probe = observer_probe(&project, cx);
        cx.run_until_parked();
        let baseline = probe.read_with(cx, |probe, _| probe.rebuilds);

        for generation in 1..=3 {
            project.update(cx, |project, cx| {
                project.on_eval_update(
                    ViewerUpdate::from_eval(EvalUpdate {
                        generation,
                        frame: generation,
                        results: vec![(NodeId::new(1), Ok(blank_display_frame(4, 4)))],
                        timings: Vec::new(),
                        scoped: Vec::new(),
                    }),
                    cx,
                );
            });
        }
        cx.run_until_parked();

        // The frames did arrive — this is not a test that nothing happened.
        assert!(matches!(
            cx.update(|cx| cx.try_global::<ViewerFrame>().cloned()),
            Some(ViewerFrame::Frame { .. })
        ));
        assert_eq!(
            probe.read_with(cx, |probe, _| probe.rebuilds),
            baseline,
            "evaluation results must not notify document-mirroring observers"
        );

        // The probe is wired: a real document change still reaches it.
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let document =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();
        assert!(
            probe.read_with(cx, |probe, _| probe.rebuilds) > baseline,
            "a document edit must still notify observers"
        );
    }

    /// RESP-2: `mirror_epoch` is the panel rebuild gate, so it must move for
    /// everything the panels mirror and stay put for everything else. A save
    /// completion is the notify that must *not* move it — it only changes the
    /// window title — and a load must move it even though `revision`
    /// deliberately does not.
    #[gpui::test]
    fn mirror_epoch_moves_for_panel_visible_changes_only(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let epoch = |cx: &mut TestAppContext| project.read_with(cx, |p, _| p.mirror_epoch());

        let dir = std::env::temp_dir().join(format!("ravel_epoch_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("epoch.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));

        // Edit.
        let before = epoch(cx);
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let document =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
        });
        let after_edit = epoch(cx);
        assert!(after_edit > before, "an edit must move the gate");

        // Undo and redo.
        project.update(cx, |project, cx| assert!(project.undo(cx)));
        let after_undo = epoch(cx);
        assert!(after_undo > after_edit, "undo must move the gate");
        project.update(cx, |project, cx| assert!(project.redo(cx)));
        assert!(epoch(cx) > after_undo, "redo must move the gate");

        // A composition switch changes what the panels show without touching
        // the document. (`create_composition` opens the new composition, so
        // switching back to the root is the bare switch.)
        let root = project.read_with(cx, |project, _| project.document().root_comp);
        project.update(cx, |project, cx| {
            project.create_composition(
                ravel_ui::document::CompositionSettings::fallback("Other"),
                cx,
            )
        });
        let before_switch = epoch(cx);
        project.update(cx, |project, cx| project.set_active_composition(root, cx));
        assert!(
            epoch(cx) > before_switch,
            "a composition switch must move the gate"
        );

        // A completed save notifies observers (the window title follows the
        // path) but changes nothing any panel mirrors.
        let before_save = epoch(cx);
        project.update(cx, |project, cx| {
            project.save_project_to(path.clone(), None, cx)
        });
        cx.run_until_parked();
        assert!(
            !project.read_with(cx, |project, _| project.is_dirty()),
            "save completed"
        );
        assert_eq!(
            epoch(cx),
            before_save,
            "a completed save must not make every panel rebuild"
        );

        // A load replaces everything the panels show. `revision` is not bumped
        // by a load application on purpose, which is exactly why the gate needs
        // its own counter.
        let before_load = epoch(cx);
        let revision_before = project.read_with(cx, |project, _| project.revision);
        project.update(cx, |project, cx| {
            project.load_project_from(path.clone(), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            project.read_with(cx, |project, _| project.revision),
            revision_before,
            "load must not bump revision (its contract)"
        );
        assert!(
            epoch(cx) > before_load,
            "a load must still move the panel gate"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// Per-node timings are the one evaluation output a panel reads outside
    /// `ViewerFrame` (the Node Editor load readout). They are published as a
    /// global for every arriving result, including one dropped as stale, so
    /// dropping the entity notify cannot stall the readout.
    ///
    /// This covers the publishing half only. That the Node Editor actually
    /// repaints from the global is `tests/eval_result_fanout.rs`, which needs
    /// a window to build the panel.
    #[gpui::test]
    fn timings_publish_even_for_a_dropped_stale_result(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        // Timings are kept only for nodes the document still has, so the two
        // measured nodes have to be real ones.
        let (first, second) = seed_two_node_layer(&project, cx);

        let update = |generation, node: NodeId, micros| {
            ViewerUpdate::from_eval(EvalUpdate {
                generation,
                frame: 0,
                results: vec![(node, Ok(blank_display_frame(4, 4)))],
                timings: vec![(node, std::time::Duration::from_micros(micros))],
                scoped: Vec::new(),
            })
        };

        project.update(cx, |project, cx| {
            project.on_eval_update(update(2, first, 500), cx)
        });
        // Generation 1 is older than the published 2, so its frame is dropped.
        project.update(cx, |project, cx| {
            project.on_eval_update(update(1, second, 900), cx)
        });

        cx.update(|cx| {
            let timings = cx.try_global::<NodeEvalTimings>().expect("timings global");
            assert_eq!(
                timings.0.get(&first).copied(),
                Some(std::time::Duration::from_micros(500))
            );
            assert_eq!(
                timings.0.get(&second).copied(),
                Some(std::time::Duration::from_micros(900)),
                "a dropped result still contributes its timings"
            );
        });
    }

    /// Add a layer holding a two-node network to the root composition and
    /// return the ids of its nodes.
    fn seed_two_node_layer(
        project: &gpui::Entity<ProjectState>,
        cx: &mut TestAppContext,
    ) -> (NodeId, NodeId) {
        let first = NodeId::next();
        let second = NodeId::next();
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let network = Graph::new()
                .add_node(
                    Node::new(first, net::NET_IN_TYPE_KEY)
                        .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR),
                )
                .unwrap()
                .add_node(
                    Node::new(second, net::NET_OUT_TYPE_KEY)
                        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
                )
                .unwrap();
            let layer = Layer::new(LayerId::next(), "Timed", network).with_time(0, 0, 300);
            let document =
                ravel_ui::document::add_layer(project.document(), comp, layer).expect("add layer");
            project.commit_document(document, InvalidationHint::Structural, cx);
        });
        (first, second)
    }

    /// The timings global is a display cache, not a log: the evaluator also
    /// measures the synthetic compositing nodes, and a deleted node keeps its
    /// last measurement forever. Both would grow the global for the whole
    /// session, so a write keeps only what the document still has
    /// (issue HIGH-21, main cause C).
    #[gpui::test]
    fn timings_never_outgrow_the_document(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let (first, second) = seed_two_node_layer(&project, cx);

        let publish = |project: &gpui::Entity<ProjectState>,
                       cx: &mut TestAppContext,
                       timings: Vec<(NodeId, std::time::Duration)>| {
            project.update(cx, |project, cx| {
                project.on_eval_update(
                    ViewerUpdate::from_eval(EvalUpdate {
                        generation: project.published_generation + 1,
                        frame: 0,
                        results: vec![(first, Ok(blank_display_frame(4, 4)))],
                        scoped: Vec::new(),
                        timings,
                    }),
                    cx,
                )
            });
        };
        let document_nodes = |project: &gpui::Entity<ProjectState>, cx: &mut TestAppContext| {
            project.read_with(cx, |project, _| document_node_ids(project.document()).len())
        };
        let stored = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                cx.try_global::<NodeEvalTimings>()
                    .map(|t| t.0.clone())
                    .unwrap_or_default()
            })
        };

        // A node the document never had (a synthetic compositing node) is not
        // stored at all.
        let ghost = NodeId::next();
        publish(
            &project,
            cx,
            vec![
                (first, std::time::Duration::from_micros(100)),
                (ghost, std::time::Duration::from_micros(200)),
            ],
        );
        let timings = stored(cx);
        assert!(timings.contains_key(&first));
        assert!(
            !timings.contains_key(&ghost),
            "a node outside the document must not be stored"
        );
        assert!(timings.len() <= document_nodes(&project, cx));

        // A node that *was* measured and is then deleted is dropped by the
        // deletion itself — not by the next evaluation result. Waiting for
        // one would leave the measurement to be inherited by whatever reuses
        // the id (a reopened project, a new node), and playback stopped at a
        // deletion publishes nothing more.
        publish(
            &project,
            cx,
            vec![(second, std::time::Duration::from_micros(300))],
        );
        assert!(stored(cx).contains_key(&second));

        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let layer = project
                .document()
                .get_composition(comp)
                .expect("root comp")
                .layers
                .iter()
                .find(|layer| layer.network.node(second).is_some())
                .expect("seeded layer")
                .id;
            let document = ravel_ui::document::remove_layer(project.document(), comp, layer)
                .expect("remove layer");
            project.commit_document(document, InvalidationHint::Structural, cx);
        });

        let timings = stored(cx);
        assert!(
            !timings.contains_key(&second),
            "the deletion itself drops the measurement"
        );
        assert!(
            !timings.contains_key(&first),
            "the whole layer is gone, so neither node survives"
        );
        assert!(timings.len() <= document_nodes(&project, cx));

        // And a result that carries no timings of its own still cannot
        // resurrect anything.
        publish(&project, cx, vec![]);
        assert!(stored(cx).is_empty());
    }

    /// Node ids are reused across documents — a persisted id is just a
    /// number, and the id counters know nothing of the document being
    /// replaced — so a measurement that survived a project load would be
    /// drawn under a completely unrelated node. Pruning cannot catch that
    /// (the id is live in both documents), so a replacement clears the
    /// readouts outright.
    #[gpui::test]
    fn a_replaced_document_does_not_inherit_readouts_through_a_reused_id(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let (first, _second) = seed_two_node_layer(&project, cx);

        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 1,
                    frame: 0,
                    results: vec![(first, Ok(blank_display_frame(4, 4)))],
                    timings: vec![(first, std::time::Duration::from_micros(500))],
                    scoped: Vec::new(),
                }),
                cx,
            )
        });
        assert!(cx.update(|cx| cx.global::<NodeEvalTimings>().0.contains_key(&first)));

        // A different document that happens to hold a node with the same id.
        project.update(cx, |project, cx| {
            let mut document = default_document(FrameRate::new(30, 1));
            let comp = document.root_comp.expect("root comp");
            let network = Graph::new()
                .add_node(
                    Node::new(first, net::NET_OUT_TYPE_KEY)
                        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
                )
                .unwrap();
            let layer = Layer::new(LayerId::next(), "Reused", network).with_time(0, 0, 300);
            document = ravel_ui::document::add_layer(&document, comp, layer).expect("add layer");
            project.replace_document(
                document,
                None,
                &UiState::with_active_comp(Some(comp)),
                SettingsLayer::default(),
                cx,
            );
        });

        assert!(
            !cx.update(|cx| cx.global::<NodeEvalTimings>().0.contains_key(&first)),
            "the previous document's measurement must not follow the id"
        );
    }

    /// The live-node scan walks every composition, every layer network and
    /// every nested subnet, so it must not run per mouse move. A scrub drag
    /// posts `InvalidationHint::Params` on every move: that moves the panel
    /// rebuild gate (`mirror_epoch`) but cannot change the node set, so it
    /// must leave `structure_epoch` — and the cached scan — alone.
    #[gpui::test]
    fn a_parameter_edit_does_not_rescan_the_document_for_node_ids(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let (first, _second) = seed_two_node_layer(&project, cx);

        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 1,
                    frame: 0,
                    results: vec![(first, Ok(blank_display_frame(4, 4)))],
                    timings: vec![(first, std::time::Duration::from_micros(500))],
                    scoped: Vec::new(),
                }),
                cx,
            )
        });
        let scanned_at = project.read_with(cx, |project, _| project.live_nodes_epoch);
        assert!(scanned_at.is_some(), "the first result scans the document");
        let (mirror, structure) = project.read_with(cx, |project, _| {
            (project.mirror_epoch, project.structure_epoch)
        });

        // Ten scrub-drag moves.
        for _ in 0..10 {
            project.update(cx, |project, cx| {
                let document = project.document().clone();
                project.apply_document(document, InvalidationHint::Params(vec![first]), cx);
            });
        }

        project.read_with(cx, |project, _| {
            assert!(
                project.mirror_epoch > mirror,
                "panels still rebuild for a parameter edit"
            );
            assert_eq!(
                project.structure_epoch, structure,
                "a parameter edit cannot change the node set"
            );
            assert_eq!(
                project.live_nodes_epoch, scanned_at,
                "so the document is not walked again"
            );
        });

        // A topology change is what invalidates the scan.
        project.update(cx, |project, cx| {
            let document = project.document().clone();
            project.commit_document(document, InvalidationHint::Structural, cx);
        });
        project.read_with(cx, |project, _| {
            assert!(project.structure_epoch > structure);
            assert_eq!(
                project.live_nodes_epoch,
                Some(project.structure_epoch),
                "the sweep re-scans at the new structure epoch"
            );
        });
    }

    #[gpui::test]
    fn dirty_tracks_edits_save_completion_and_new_baseline(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        let dir = std::env::temp_dir().join(format!("ravel_dirty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dirty.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));

        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let document =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
            assert!(project.is_dirty());
            project.save_project_to(path.clone(), None, cx);
            // A request alone is not a completed save.
            assert!(project.is_dirty());
        });
        cx.run_until_parked();
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        project.update(cx, |project, cx| project.new_document(cx));
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    #[gpui::test]
    fn edit_after_save_request_remains_dirty(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_stale_save_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stale.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));

        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let one_layer =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(one_layer, InvalidationHint::Structural, cx);
            project.save_project_to(path.clone(), None, cx);

            let two_layers =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(two_layers, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        assert!(project.read_with(cx, |project, _| project.is_dirty()));
        let saved = ravel_project::ProjectFile::load(&path).unwrap();
        assert_eq!(
            ravel_ui::document::root_composition(&saved.document)
                .unwrap()
                .layer_count(),
            1,
            "the completed save must retain its request-time snapshot"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    #[gpui::test]
    fn guarded_save_callback_waits_behind_an_in_flight_save(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_guard_queue_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.ravprj");
        let guarded = dir.join("guarded.ravprj");
        for path in [&first, &guarded] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }

        let outcome = std::rc::Rc::new(std::cell::Cell::new(None));
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let document =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
            project.save_project_to(first.clone(), None, cx);
            let callback_outcome = outcome.clone();
            project.save_project_to_then(
                guarded.clone(),
                None,
                move |result, _cx| callback_outcome.set(Some(result)),
                cx,
            );
            assert_eq!(outcome.get(), None);
        });
        cx.run_until_parked();

        assert_eq!(outcome.get(), Some(SaveOutcome::Saved));
        assert!(first.exists());
        assert!(guarded.exists());
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        for path in [&first, &guarded] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Save → New → Load restores layers, keyframes, and custom parameters;
    /// loading replaces the undo history wholesale (REQ-LAYER-009).
    #[gpui::test]
    fn save_new_load_roundtrips_the_document(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_project_state_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));

        // Commit content, then save (the write completes on the test
        // dispatcher).
        let saved = project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path.clone(), None, cx);
            project.document().clone()
        });
        cx.run_until_parked();
        project.read_with(cx, |project, _| {
            assert_eq!(project.project_path(), Some(path.as_path()));
            assert!(!project.is_dirty());
        });

        // File ▸ New: default document, cleared path, fresh undo history.
        project.update(cx, |project, cx| {
            project.new_document(cx);
            assert!(project.project_path().is_none());
            assert!(!project.is_dirty());
            assert!(!project.undo(cx), "a new document has no undo history");
            assert_eq!(project.active_composition(cx).unwrap().layer_count(), 0);
        });

        // File ▸ Open: the saved content is restored exactly.
        project.update(cx, |project, cx| {
            project.load_project_from(path.clone(), cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| {
            assert_eq!(project.document(), &saved);
            assert_eq!(project.project_path(), Some(path.as_path()));
            assert!(!project.is_dirty());
            assert!(!project.undo(cx), "loading is not an undo step");
        });

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// A failed load keeps the current document and path untouched.
    #[gpui::test]
    fn failed_load_keeps_the_current_document(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let before = project.read_with(cx, |project, _| project.document().clone());

        let missing = std::env::temp_dir().join("ravel_definitely_missing_12345.ravprj");
        let _ = std::fs::remove_file(&missing);
        project.update(cx, |project, cx| {
            project.load_project_from(missing, cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            assert_eq!(project.document(), &before);
            assert!(project.project_path().is_none());
        });
    }

    /// A save whose write finishes after File ▸ New must not adopt its path
    /// onto the fresh document (the path describes different content).
    #[gpui::test]
    fn save_completing_after_new_does_not_adopt_the_path(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_project_race_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("race.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));

        project.update(cx, |project, cx| {
            project.save_project_to(path.clone(), None, cx);
            // New replaces the document identity before the write lands.
            project.new_document(cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            assert!(project.project_path().is_none());
        });

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// A load whose read finishes after an intervening edit is discarded
    /// rather than silently dropping the user's edit.
    #[gpui::test]
    fn load_completing_after_an_edit_is_discarded(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_load_race_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("load_race.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));

        // Save a document with one layer, then start over.
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path.clone(), None, cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| project.new_document(cx));

        // Start loading, then edit before the read completes.
        project.update(cx, |project, cx| {
            project.load_project_from(path.clone(), cx);
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, cx| {
            // The edit survived; the in-flight load was discarded.
            assert!(project.project_path().is_none());
            assert_eq!(project.active_composition(cx).unwrap().layer_count(), 1);
        });

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(ravel_project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// A save requested while another is in flight is queued, not run
    /// concurrently: both files are written in order and the final adopted
    /// path is the last request's.
    #[gpui::test]
    fn concurrent_saves_are_serialized(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_save_queue_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.ravprj");
        let second = dir.join("second.ravprj");
        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }

        project.update(cx, |project, cx| {
            project.save_project_to(first.clone(), None, cx);
            project.save_project_to(second.clone(), None, cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            assert_eq!(project.project_path(), Some(second.as_path()));
        });
        assert!(first.exists());
        assert!(second.exists());

        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// A queued save writes the document snapshot from request time: a New
    /// between request and execution must neither change what the queued
    /// save writes nor let it adopt the path.
    #[gpui::test]
    fn queued_save_uses_the_request_time_snapshot(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_save_snap_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.ravprj");
        let second = dir.join("second.ravprj");
        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }

        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            // A runs; B is queued. Both snapshot the one-layer document.
            project.save_project_to(first.clone(), None, cx);
            project.save_project_to(second.clone(), None, cx);
            // New replaces the document before B executes.
            project.new_document(cx);
        });
        cx.run_until_parked();

        // B wrote the request-time snapshot (one layer), not the empty
        // replacement document; neither save adopted its path.
        project.read_with(cx, |project, _| {
            assert!(project.project_path().is_none());
        });
        let loaded_b = ravel_project::ProjectFile::load(&second).unwrap();
        let root_b =
            ravel_ui::document::root_composition(&loaded_b.document).expect("root comp in B");
        assert_eq!(root_b.layer_count(), 1, "B must contain the old document");

        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Three rapid saves queue all destinations; none is silently dropped.
    #[gpui::test]
    fn a_third_save_does_not_overwrite_the_second(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_save_third_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<_> = ["one.ravprj", "two.ravprj", "three.ravprj"]
            .iter()
            .map(|name| dir.join(name))
            .collect();
        for path in &paths {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }

        project.update(cx, |project, cx| {
            for path in &paths {
                project.save_project_to(path.clone(), None, cx);
            }
        });
        cx.run_until_parked();

        for path in &paths {
            assert!(path.exists(), "{} was written", path.display());
        }
        project.read_with(cx, |project, _| {
            assert_eq!(project.project_path(), Some(paths[2].as_path()));
        });

        for path in &paths {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Overlapping File ▸ Open requests resolve latest-wins: the earlier
    /// load is discarded even if it completes first.
    #[gpui::test]
    fn overlapping_loads_resolve_latest_wins(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_load_wins_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path_a = dir.join("a.ravprj");
        let path_b = dir.join("b.ravprj");
        for path in [&path_a, &path_b] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }

        // File A: one layer. File B: two layers.
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path_a.clone(), None, cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path_b.clone(), None, cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| project.new_document(cx));

        // Request A, then B, before either completes.
        project.update(cx, |project, cx| {
            project.load_project_from(path_a.clone(), cx);
            project.load_project_from(path_b.clone(), cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, cx| {
            assert_eq!(project.project_path(), Some(path_b.as_path()));
            assert_eq!(project.active_composition(cx).unwrap().layer_count(), 2);
        });

        for path in [&path_a, &path_b] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(ravel_project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Reproduces the stale-viewer bug: a structural edit (Geometry node
    /// deleted upstream of a Rasterize) makes the evaluation fail, and the
    /// error must replace the previously shown frame instead of leaving it
    /// on screen; a later successful evaluation restores normal drawing.
    /// `CM-7`: a frame that reaches the host still linear means the display
    /// transform did not run — a shader that will not compile, a lost device.
    /// The viewer must say so rather than blank, which is what it did before
    /// the error was surfaced. A `Scalar` still blanks: that is not a frame
    /// anyone expected to see.
    #[gpui::test]
    fn an_untransformed_frame_becomes_a_viewer_error(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let update = |generation, value: Arc<dyn ravel_core::types::NodeData>| {
            ViewerUpdate::from_eval(EvalUpdate {
                generation,
                frame: 0,
                results: vec![(NodeId::new(1), Ok(value))],
                timings: Vec::new(),
                scoped: Vec::new(),
            })
        };

        project.update(cx, |project, cx| {
            project.on_eval_update(update(1, Arc::new(FrameBuffer::new_zeroed(4, 4))), cx)
        });
        project.read_with(cx, |_, cx| match cx.try_global::<ViewerFrame>() {
            Some(ViewerFrame::Error { message, .. }) => assert_eq!(
                message.as_ref(),
                ravel_i18n::translate("viewer.display_transform_failed"),
                "the error must be the localized display-transform message",
            ),
            other => panic!("expected an error overlay, got {other:?}"),
        });

        project.update(cx, |project, cx| {
            project.on_eval_update(update(2, Arc::new(ravel_core::types::Scalar(1.0))), cx)
        });
        project.read_with(cx, |_, cx| {
            assert!(
                matches!(
                    cx.try_global::<ViewerFrame>(),
                    Some(ViewerFrame::Blank { .. })
                ),
                "a non-frame output still blanks",
            )
        });
    }

    #[gpui::test]
    fn eval_error_replaces_the_frame_and_recovers(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let ok_update = |generation| {
            ViewerUpdate::from_eval(EvalUpdate {
                generation,
                frame: 0,
                results: vec![(NodeId::new(1), Ok(blank_display_frame(4, 4)))],
                timings: Vec::new(),
                scoped: Vec::new(),
            })
        };
        let err_update = |generation| {
            ViewerUpdate::from_eval(EvalUpdate {
                generation,
                frame: 0,
                results: vec![(
                    NodeId::new(1),
                    Err(ravel_core::eval::EvalError::NodeNotFound(NodeId::new(42))),
                )],
                timings: Vec::new(),
                scoped: Vec::new(),
            })
        };

        // A good frame is published.
        project.update(cx, |project, cx| project.on_eval_update(ok_update(1), cx));
        project.read_with(cx, |project, cx| match cx.try_global::<ViewerFrame>() {
            Some(ViewerFrame::Frame {
                image,
                composition_resolution,
            }) => {
                assert_eq!((image.width(), image.height()), (4, 4));
                assert_eq!(
                    *composition_resolution,
                    project.active_composition(cx).unwrap().resolution
                );
            }
            other => panic!("expected a published frame, got {other:?}"),
        });

        // The structural edit makes the evaluation fail: the error replaces
        // the frame (before the fix, the stale frame was kept).
        project.update(cx, |project, cx| project.on_eval_update(err_update(2), cx));
        project.read_with(cx, |project, cx| match cx.try_global::<ViewerFrame>() {
            Some(ViewerFrame::Error {
                message,
                composition_resolution,
            }) => {
                assert!(message.contains("42"), "unexpected message: {message}");
                // The error carries the full composition resolution so the
                // panel can share normal output's viewport geometry.
                let comp = project.active_composition(cx).expect("root comp");
                assert_eq!(*composition_resolution, Some(comp.resolution));
            }
            other => panic!("expected an error state, got {other:?}"),
        });

        // Fixing the graph restores normal drawing.
        project.update(cx, |project, cx| project.on_eval_update(ok_update(3), cx));
        project.read_with(cx, |_, cx| match cx.try_global::<ViewerFrame>() {
            Some(ViewerFrame::Frame { .. }) => {}
            other => panic!("expected recovery to a frame, got {other:?}"),
        });
    }

    /// A non-frame (e.g. Geometry) successful output blanks the viewer.
    #[gpui::test]
    fn non_frame_output_blanks_the_viewer(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 1,
                    frame: 0,
                    results: vec![(NodeId::new(1), Ok(Arc::new(ravel_core::types::Scalar(1.0))))],
                    timings: Vec::new(),
                    scoped: Vec::new(),
                }),
                cx,
            );
        });
        project.read_with(cx, |project, cx| {
            let expected = project.active_composition(cx).unwrap().resolution;
            assert!(matches!(
                cx.try_global::<ViewerFrame>(),
                Some(ViewerFrame::Blank {
                    composition_resolution: Some(resolution),
                }) if *resolution == expected
            ));
        });
    }

    /// Monotonic publication (generation-starvation regression): a result
    /// that is newer than the displayed one is published even when it is no
    /// longer the very latest request, and an older in-flight result can
    /// never overwrite a newer one.
    #[gpui::test]
    fn newer_results_publish_and_older_ones_never_regress(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        // Distinguish generations by frame size.
        let update = |generation, size| {
            ViewerUpdate::from_eval(EvalUpdate {
                generation,
                frame: 0,
                results: vec![(NodeId::new(1), Ok(blank_display_frame(size, size)))],
                timings: Vec::new(),
                scoped: Vec::new(),
            })
        };
        let shown_size = |cx: &mut TestAppContext| {
            project.read_with(cx, |_, cx| match cx.try_global::<ViewerFrame>() {
                Some(ViewerFrame::Frame { image, .. }) => image.width(),
                other => panic!("expected a frame, got {other:?}"),
            })
        };

        project.update(cx, |project, cx| project.on_eval_update(update(1, 1), cx));
        assert_eq!(shown_size(cx), 1);

        // Under load the coalescing window has moved on (a newer request
        // exists), but this completed result is still newer than what is on
        // screen: it must be published, not starved.
        project.update(cx, |project, cx| project.on_eval_update(update(3, 3), cx));
        assert_eq!(shown_size(cx), 3);

        // An older in-flight result arriving late must not move the display
        // backwards.
        project.update(cx, |project, cx| project.on_eval_update(update(2, 2), cx));
        assert_eq!(shown_size(cx), 3);
    }

    /// A path that blanks the viewer evaluates nothing, so nothing will
    /// arrive to replace the snapshot: it has to be dropped on the way out or
    /// the overlays keep drawing the last composition's results over a blank
    /// frame.
    #[gpui::test]
    fn a_path_with_no_evaluation_drops_the_overlay_results(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let stale = NodeId::new(9001);
        cx.update(|cx| {
            cx.set_global(EvalResults::new(HashMap::from([(
                (
                    vec![PathSegment::Layer(CompId::new(1), LayerId::new(1))],
                    stale,
                ),
                Arc::new(ravel_core::types::Scalar(1.0)) as Arc<dyn ravel_core::types::NodeData>,
            )])));
        });

        // No active composition: `build_viewer_request` returns `Ok(None)`.
        cx.update(|cx| crate::panels::set_active_composition(None, cx));
        project.update(cx, |project, cx| {
            project.request_viewer_eval(InvalidationHint::None, cx);
        });

        cx.update(|cx| {
            assert!(
                cx.global::<EvalResults>().values.is_empty(),
                "the blank path kept the results of the composition before it"
            );
        });
    }

    /// The overlay snapshot is published with the frame it belongs to: an
    /// accepted update installs its targets, a stale one changes nothing, and
    /// an accepted update whose target did not return leaves no earlier value
    /// behind for an overlay to paint.
    #[gpui::test]
    fn overlay_results_track_the_published_frame(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let overlay_node = NodeId::new(9001);
        let frame = blank_display_frame(4, 4);
        let scalar: Arc<dyn ravel_core::types::NodeData> = Arc::new(ravel_core::types::Scalar(2.0));
        let overlay_values = |cx: &App| {
            cx.try_global::<EvalResults>()
                .map(|results| {
                    results
                        .values
                        .keys()
                        .map(|(_, node)| *node)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 2,
                    frame: 10,
                    results: vec![(NodeId::new(1), Ok(frame.clone()))],
                    timings: Vec::new(),
                    scoped: vec![scoped_result(overlay_node, Ok(scalar.clone()))],
                }),
                cx,
            );
        });
        project.read_with(cx, |_, cx| {
            assert_eq!(overlay_values(cx), vec![overlay_node]);
        });

        // Dropped as older than the published frame, so it must not install
        // its own targets either.
        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 1,
                    frame: 9,
                    results: vec![(NodeId::new(1), Ok(frame.clone()))],
                    timings: Vec::new(),
                    scoped: vec![scoped_result(NodeId::new(9002), Ok(scalar.clone()))],
                }),
                cx,
            );
        });
        project.read_with(cx, |_, cx| {
            assert_eq!(overlay_values(cx), vec![overlay_node]);
        });

        // A target that failed contributes an `Err`, which is not a value: the
        // previous frame's result must not stand in for it.
        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 3,
                    frame: 11,
                    results: vec![(NodeId::new(1), Ok(frame))],
                    timings: Vec::new(),
                    scoped: vec![scoped_result(
                        overlay_node,
                        Err(ravel_core::eval::EvalError::MissingProcessor(overlay_node)),
                    )],
                }),
                cx,
            );
        });
        project.read_with(cx, |_, cx| {
            assert!(overlay_values(cx).is_empty());
        });
    }

    /// An overlay annotates the frame under it. When target 0 fails or is not
    /// a frame the viewer shows an error or a blank, so the later targets'
    /// values — however successful — describe a composition that is not on
    /// screen and must not be published.
    #[gpui::test]
    fn overlay_results_are_withheld_when_the_frame_is_not_an_image(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let overlay_node = NodeId::new(9001);
        let scalar: Arc<dyn ravel_core::types::NodeData> = Arc::new(ravel_core::types::Scalar(2.0));
        let overlay_is_empty = |cx: &App| {
            cx.try_global::<EvalResults>()
                .is_none_or(|results| results.values.is_empty())
        };

        // Target 0 failed: the viewer publishes an error frame.
        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 1,
                    frame: 10,
                    results: vec![
                        (
                            NodeId::new(1),
                            Err(ravel_core::eval::EvalError::MissingProcessor(NodeId::new(
                                1,
                            ))),
                        ),
                        (overlay_node, Ok(scalar.clone())),
                    ],
                    timings: Vec::new(),
                    scoped: Vec::new(),
                }),
                cx,
            );
        });
        project.read_with(cx, |_, cx| {
            assert!(
                matches!(
                    cx.global::<crate::panels::ViewerFrame>(),
                    crate::panels::ViewerFrame::Error { .. }
                ),
                "this test needs the error frame it is written against",
            );
            assert!(
                overlay_is_empty(cx),
                "an overlay result was published over an error frame",
            );
        });

        // Target 0 is not a frame at all: the viewer blanks.
        project.update(cx, |project, cx| {
            project.on_eval_update(
                ViewerUpdate::from_eval(EvalUpdate {
                    generation: 2,
                    frame: 11,
                    results: vec![
                        (NodeId::new(1), Ok(scalar.clone())),
                        (overlay_node, Ok(scalar.clone())),
                    ],
                    timings: Vec::new(),
                    scoped: Vec::new(),
                }),
                cx,
            );
        });
        project.read_with(cx, |_, cx| {
            assert!(
                matches!(
                    cx.global::<crate::panels::ViewerFrame>(),
                    crate::panels::ViewerFrame::Blank { .. }
                ),
                "this test needs the blank frame it is written against",
            );
            assert!(
                overlay_is_empty(cx),
                "an overlay result was published over a blank frame",
            );
        });
    }
}

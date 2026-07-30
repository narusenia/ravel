// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Audio playback wiring: document observer → mixer tracks → CPAL engine
//! (`docs/implementation/audio-plan.md`, unit 3).
//!
//! [`AudioService`] owns the optional [`AudioEngine`] and turns document
//! changes into mixer commands:
//!
//! - [`ProjectState`](crate::project_state::ProjectState) funnels every
//!   document mutation through [`sync_from_document`]; the service diffs
//!   [`AudioMixdown::desired_tracks`] against what it last sent and emits
//!   only `SetTrack` / `RemoveTrack` changes. Source audio is prepared at the
//!   output rate before it enters the cache, so later placement edits reach
//!   the mixer at the next mixed block without repeating SRC.
//! - Source audio is decoded and resampled on the background executor (never
//!   the UI thread) and cached per asset + stream, per decision 8 of the plan
//!   (full-length decode, memory-resident, warn-and-skip past
//!   [`mixdown::MAX_DECODE_BYTES`]).
//! - The engine starts lazily on the first audio layer and its absence is
//!   a fallback, not an error: with no output device (CI, headless tests)
//!   playback simply stays on the wall clock.
//!
//! The playback clock switch (decision 4) has exactly one decision point,
//! [`playback_clock`]: an audio clock is used while the active composition
//! has audio tracks **and** an engine is running; everything else falls
//! back to `ClockSource::Wall`.

pub mod mixdown;

use gpui::{App, Context, Entity, Global, WeakEntity};
use mixdown::{AudioMixdown, CacheKey, DecodedAudio, TrackSpec};
use ravel_audio::{
    AudioCommand, AudioEngine, AudioEngineConfig, AudioError, OutputConfig, SyncClock, Track,
};
use ravel_core::composition::Document;
use ravel_core::id::LayerId;
use ravel_core::types::FrameRate;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

/// Destination for mixer commands: the real engine in production, a
/// recording stub in tests. Abstracted so the diff/clock logic is testable
/// without an audio device. Used only on the UI thread (`cpal::Stream` is
/// platform-dependently `!Send`, so no `Send` bound here).
pub trait AudioSink {
    /// Forward one command to the engine's prep thread.
    fn send(&self, command: AudioCommand) -> Result<(), AudioError>;
    /// The shared playback clock (used only while audio tracks exist).
    fn sync_clock(&self) -> Arc<SyncClock>;
}

/// Sink backed by a running [`AudioEngine`]. `Rc`, not `Arc`: the engine
/// (and its `cpal::Stream`) is platform-dependently `!Send`/`!Sync` and
/// only ever touched on the UI thread.
struct EngineSink(Rc<AudioEngine>);

impl AudioSink for EngineSink {
    fn send(&self, command: AudioCommand) -> Result<(), AudioError> {
        self.0.send(command)
    }

    fn sync_clock(&self) -> Arc<SyncClock> {
        self.0.sync_clock().clone()
    }
}

/// What was last sent to the mixer for one layer, for diffing.
struct SentTrack {
    spec: TrackSpec,
    /// `false` while the source audio is still being decoded: the spec is
    /// recorded (so a repeated document change does not spawn a second
    /// decode) but nothing has reached the mixer yet.
    delivered: bool,
    /// Last built track, reused for cheap updates that only move the layer
    /// on the timeline or toggle mute/solo/fades (no re-slice, no gain
    /// curve re-sampling — important while a layer is dragged).
    built: Option<Track>,
}

/// GPUI entity owning the audio engine, the decode cache, and the track
/// diff state. One per app session, registered through
/// [`AudioServiceHandle`]; sessions without the handle (unit tests) simply
/// get no audio, which is the designed fallback.
pub struct AudioService {
    sink: Option<Box<dyn AudioSink>>,
    /// Engine startup failed once (no device); do not retry on every edit.
    engine_unavailable: bool,
    /// Sample rate the engine runs at; every placement value is converted
    /// into these frames (see [`mixdown`]).
    output_rate: u32,
    /// Fully decoded source audio per asset + stream (decision 8).
    cache: HashMap<CacheKey, Arc<DecodedAudio>>,
    /// Assets that failed to decode (offline, over the memory cap, …); not
    /// retried until the document is replaced.
    failed: HashSet<CacheKey>,
    /// Decodes currently in flight on the background executor.
    pending: HashSet<CacheKey>,
    /// Last state sent to the mixer, per layer.
    sent: HashMap<LayerId, SentTrack>,
    /// Audio tracks in the active composition — the track-count half of
    /// the clock-switch condition.
    desired_count: usize,
    /// Bumped on document replacement so in-flight decodes of the previous
    /// document cannot populate the cache (asset ids may be reused across
    /// documents for different files).
    generation: u64,
}

impl AudioService {
    /// Create the service without an engine; the engine starts lazily when
    /// the first audio track appears, so sessions that never use audio
    /// (including every existing UI test) never open a device.
    pub fn new() -> Self {
        Self::with_sink(None, OutputConfig::default().sample_rate)
    }

    /// Create the service with a pre-installed sink (`None` = the real
    /// engine on first use, `Some` = a stub for tests).
    pub fn with_sink(sink: Option<Box<dyn AudioSink>>, output_rate: u32) -> Self {
        Self {
            sink,
            engine_unavailable: false,
            output_rate,
            cache: HashMap::new(),
            failed: HashSet::new(),
            pending: HashSet::new(),
            sent: HashMap::new(),
            desired_count: 0,
            generation: 0,
        }
    }

    /// The shared sync clock while the audio path should drive playback:
    /// audio tracks exist in the active composition and an engine (or
    /// stub) is running. This is the track-count half of the single
    /// clock-switch decision point; see [`playback_clock`].
    pub fn audio_clock(&self) -> Option<Arc<SyncClock>> {
        if self.desired_count == 0 {
            return None;
        }
        self.sink.as_ref().map(|sink| sink.sync_clock())
    }

    /// Insert already-decoded audio into the cache (used by the decode
    /// completion path and by tests).
    pub fn cache_decoded(&mut self, key: CacheKey, audio: DecodedAudio) {
        debug_assert_eq!(audio.sample_rate, self.output_rate);
        self.cache.insert(key, Arc::new(audio));
    }

    /// Mirror the transport into the engine: seek first (so a resume
    /// continues from the right position), then play/pause. Sent whenever
    /// the transport changes, even with zero audio tracks, so the clock is
    /// already aligned when a track appears mid-playback.
    pub fn forward_transport(&self, playing: bool, seek_secs: Option<f64>) {
        let Some(sink) = &self.sink else {
            return;
        };
        if let Some(secs) = seek_secs {
            let _ = sink.send(AudioCommand::Seek(secs));
        }
        let command = if playing {
            AudioCommand::Play
        } else {
            AudioCommand::Pause
        };
        let _ = sink.send(command);
    }

    /// Forget document-derived state on project New/Open: asset ids may be
    /// reused for different files, so the cache, the failure set, and all
    /// mixer tracks are dropped. In-flight decodes are discarded through
    /// the generation counter when they complete.
    pub fn on_document_replaced(&mut self) {
        self.generation += 1;
        self.cache.clear();
        self.failed.clear();
        let removed: Vec<LayerId> = self.sent.keys().copied().collect();
        self.sent.clear();
        for id in removed {
            self.send(AudioCommand::RemoveTrack(id.raw()));
        }
    }

    /// Diff the active composition's audio layers against the mixer state
    /// and send only the changes. Called from
    /// [`ProjectState`](crate::project_state::ProjectState)'s document
    /// observer on every edit, undo/redo, and composition switch.
    pub fn sync(&mut self, document: &Document, cx: &mut Context<Self>) {
        let comp = crate::panels::active_composition_in(document, cx);
        let comp_fps = comp.map(|c| c.frame_rate).unwrap_or(FrameRate::new(30, 1));
        let mut desired = comp
            .map(|comp| AudioMixdown::desired_tracks(comp, self.output_rate))
            .unwrap_or_default();
        self.desired_count = desired.len();
        if self.desired_count > 0 {
            let previous_rate = self.output_rate;
            self.ensure_engine(cx);
            if self.output_rate != previous_rate {
                desired = comp
                    .map(|comp| AudioMixdown::desired_tracks(comp, self.output_rate))
                    .unwrap_or_default();
                self.desired_count = desired.len();
            }
        }

        // Removals first: a layer that lost its audio (or left the
        // composition) leaves the mixer immediately.
        let desired_ids: HashSet<LayerId> = desired.iter().map(|spec| spec.layer_id).collect();
        let removed: Vec<LayerId> = self
            .sent
            .keys()
            .copied()
            .filter(|id| !desired_ids.contains(id))
            .collect();
        for id in removed {
            self.sent.remove(&id);
            self.send(AudioCommand::RemoveTrack(id.raw()));
        }

        for spec in desired {
            if let Some(sent) = self.sent.get(&spec.layer_id) {
                if sent.delivered && sent.spec == spec {
                    continue; // Nothing changed: no command.
                }
                // Cheap update (timeline drag, mute/solo/fade): patch the
                // placement fields on the previously built track.
                if sent.delivered
                    && spec.shares_build_with(&sent.spec)
                    && let Some(built) = &sent.built
                {
                    let mut track = built.clone();
                    track.start_frame = spec.start_frame as usize;
                    track.muted = spec.muted;
                    track.solo = spec.solo;
                    track.fade_in_frames = spec.fade_in_frames as usize;
                    track.fade_out_frames = spec.fade_out_frames as usize;
                    self.send(AudioCommand::SetTrack(track.clone()));
                    self.sent.insert(
                        spec.layer_id,
                        SentTrack {
                            spec,
                            delivered: true,
                            built: Some(track),
                        },
                    );
                    continue;
                }
            }

            let key = spec.cache_key();
            match self.cache.get(&key).cloned() {
                Some(decoded) => {
                    match AudioMixdown::build_track(&spec, &decoded, comp_fps, self.output_rate) {
                        Some(track) => {
                            self.send(AudioCommand::SetTrack(track.clone()));
                            self.sent.insert(
                                spec.layer_id,
                                SentTrack {
                                    spec,
                                    delivered: true,
                                    built: Some(track),
                                },
                            );
                        }
                        // Trimmed to nothing: the layer must be silent.
                        None => {
                            self.send(AudioCommand::RemoveTrack(spec.layer_id.raw()));
                            self.sent.insert(
                                spec.layer_id,
                                SentTrack {
                                    spec,
                                    delivered: true,
                                    built: None,
                                },
                            );
                        }
                    }
                }
                None => {
                    // Record the spec now so further edits do not spawn
                    // duplicate decodes; delivery happens when the decode
                    // completes and triggers a re-sync.
                    self.sent.insert(
                        spec.layer_id,
                        SentTrack {
                            spec: spec.clone(),
                            delivered: false,
                            built: None,
                        },
                    );
                    self.request_decode(document, &spec, cx);
                }
            }
        }
    }

    /// Re-run the diff against the live document (decode completions land
    /// here, so a track always goes out with its freshest spec, not the
    /// spec captured when the decode started).
    fn resync_from_project(&mut self, cx: &mut Context<Self>) {
        let document = cx
            .try_global::<crate::project_state::ProjectStateHandle>()
            .and_then(|handle| handle.0.upgrade())
            .map(|project| project.read(cx).document().clone());
        if let Some(document) = document {
            self.sync(&document, cx);
        }
    }

    fn send(&self, command: AudioCommand) {
        let Some(sink) = &self.sink else {
            return;
        };
        if let Err(err) = sink.send(command) {
            tracing::warn!(error = %err, "audio command dropped");
        }
    }

    /// Start the real engine on first use and align its clock with the
    /// current transport, so switching to the audio clock mid-playback
    /// does not jump the playhead. A missing device is a fallback, not an
    /// error: playback stays on the wall clock.
    fn ensure_engine(&mut self, cx: &App) {
        if self.sink.is_some() || self.engine_unavailable {
            return;
        }
        match AudioEngine::new(AudioEngineConfig::default()) {
            Ok(engine) => {
                self.output_rate = engine.output_config().sample_rate;
                let engine = Rc::new(engine);
                let position = cx
                    .try_global::<crate::panels::PlaybackPosition>()
                    .copied()
                    .unwrap_or_default();
                let fps = position.fps;
                let secs = position.frame as f64 * fps.den.max(1) as f64 / fps.num.max(1) as f64;
                let _ = engine.seek(secs);
                let playing = cx
                    .try_global::<crate::playback::PlaybackControllerHandle>()
                    .and_then(|handle| handle.0.upgrade())
                    .map(|controller| controller.read(cx).transport().is_playing())
                    .unwrap_or(false);
                if playing {
                    let _ = engine.play();
                }
                tracing::info!("audio engine started");
                self.sink = Some(Box::new(EngineSink(engine)));
            }
            Err(err) => {
                tracing::warn!(error = %err, "audio output unavailable; playback uses the wall clock");
                self.engine_unavailable = true;
            }
        }
    }

    /// Spawn a background decode for `spec`'s source, unless one is already
    /// cached, in flight, or known to fail. Completion re-syncs so the
    /// track goes out with the freshest spec.
    fn request_decode(&mut self, document: &Document, spec: &TrackSpec, cx: &mut Context<Self>) {
        let key = spec.cache_key();
        if self.pending.contains(&key) || self.failed.contains(&key) {
            return;
        }
        let Some(entry) = document.media_assets.get(&spec.asset_id) else {
            tracing::warn!(
                asset_id = spec.asset_id,
                "audio layer references an unknown media asset; track skipped"
            );
            self.failed.insert(key);
            return;
        };
        let Some(path) = entry.resolved.clone() else {
            tracing::warn!(
                asset_id = spec.asset_id,
                "media asset is offline; audio track skipped"
            );
            self.failed.insert(key);
            return;
        };

        self.pending.insert(key.clone());
        let generation = self.generation;
        let stream_index = spec.stream_index;
        let output_rate = self.output_rate;
        let decode = cx.background_executor().spawn(async move {
            let audio = mixdown::decode_full_audio(&path, stream_index)?;
            mixdown::prepare_audio_at_rate(audio, output_rate)
        });
        cx.spawn(async move |this, cx| {
            let result = decode.await;
            let _ = this.update(cx, |this, cx| {
                this.pending.remove(&key);
                if this.generation == generation {
                    match result {
                        Ok(audio) => {
                            this.cache.insert(key, Arc::new(audio));
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "audio decode failed; track skipped");
                            this.failed.insert(key);
                        }
                    }
                }
                // Re-sync even on a generation mismatch: the new document's
                // diff decides what to decode next.
                this.resync_from_project(cx);
            });
        })
        .detach();
    }
}

impl Default for AudioService {
    fn default() -> Self {
        Self::new()
    }
}

/// Durable registry of the app's single [`AudioService`], resolved by the
/// project state's document observer and the playback controller. Sessions
/// without the handle (unit tests that never register it) get no audio —
/// the designed fallback.
pub struct AudioServiceHandle(pub WeakEntity<AudioService>);

impl Global for AudioServiceHandle {}

fn service(cx: &App) -> Option<Entity<AudioService>> {
    cx.try_global::<AudioServiceHandle>()?.0.upgrade()
}

/// The single decision point for the playback clock switch (decision 4 of
/// the plan): `Some(sync_clock)` — playback follows the audio device — iff
/// the active composition has audio tracks and an engine is running.
/// Everything else (no tracks, no device, no service) returns `None` and
/// the transport uses `ClockSource::Wall`.
pub fn playback_clock(cx: &App) -> Option<Arc<SyncClock>> {
    service(cx)?.read(cx).audio_clock()
}

/// Forward a transport change to the audio engine (no-op without one).
pub fn forward_transport(playing: bool, seek_secs: Option<f64>, cx: &App) {
    if let Some(service) = service(cx) {
        service.read(cx).forward_transport(playing, seek_secs);
    }
}

/// The project state's document observer entry point: diff the new
/// document against the mixer and send the changes.
pub fn sync_from_document(document: &Document, cx: &mut App) {
    if let Some(service) = service(cx) {
        service.update(cx, |service, cx| service.sync(document, cx));
    }
}

/// Project New/Open hook: drop document-derived audio state before the
/// replacement document's first sync.
pub fn document_replaced(cx: &mut App) {
    if let Some(service) = service(cx) {
        service.update(cx, |service, _cx| service.on_document_replaced());
    }
}

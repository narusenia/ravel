// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Audio playback wiring tests (`docs/implementation/audio-plan.md`, unit 3).
//!
//! The real CPAL engine cannot run in CI (no output device), so these tests
//! drive [`AudioService`] with a recording stub sink and verify:
//!
//! - an audio layer on the document produces a `SetTrack` command whose
//!   content matches the layer (the "sound comes out" path, fixed at the
//!   command boundary),
//! - mid-playback edits send minimal diffs (`SetTrack` replacement, never a
//!   remove/add gap),
//! - the playback clock switch: audio clock with tracks + engine, wall
//!   clock otherwise.

use core::prelude::v1::test;

use gpui::{AppContext as _, TestAppContext};
use ravel_app::audio::mixdown::{CacheKey, DecodedAudio};
use ravel_app::audio::{AudioService, AudioServiceHandle, AudioSink};
use ravel_app::playback::{ClockSource, Transport};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_audio::{AudioCommand, AudioError, SyncClock};
use ravel_core::composition::{AudioSource, Layer};
use ravel_core::graph::Graph;
use ravel_core::id::LayerId;
use ravel_core::runtime::InvalidationHint;
use ravel_core::runtime::playback::PlaybackState;
use ravel_core::types::FrameRate;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FPS: FrameRate = FrameRate { num: 30, den: 1 };

// ---------------------------------------------------------------------------
// Recording stub sink
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Recorded {
    SetTrack {
        id: u64,
        start_frame: usize,
        muted: bool,
        solo: bool,
        sample_rate: u32,
    },
    RemoveTrack(u64),
}

#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Vec<Recorded>>>);

impl Recording {
    fn commands(&self) -> Vec<Recorded> {
        self.0.lock().unwrap().clone()
    }
}

struct StubSink {
    recording: Recording,
    clock: Arc<SyncClock>,
}

impl AudioSink for StubSink {
    fn send(&self, command: AudioCommand) -> Result<(), AudioError> {
        let recorded = match command {
            AudioCommand::SetTrack { track, sample_rate } => Recorded::SetTrack {
                id: track.id,
                start_frame: track.start_frame,
                muted: track.muted,
                solo: track.solo,
                sample_rate,
            },
            AudioCommand::RemoveTrack(id) => Recorded::RemoveTrack(id),
            // Transport forwarding is not exercised through this sink.
            _ => return Ok(()),
        };
        self.recording.0.lock().unwrap().push(recorded);
        Ok(())
    }

    fn sync_clock(&self) -> Arc<SyncClock> {
        self.clock.clone()
    }
}

fn decoded(frames: usize, channels: u32, sample_rate: u32) -> DecodedAudio {
    DecodedAudio {
        samples: vec![0.25; frames * channels as usize].into(),
        sample_rate,
        channels,
    }
}

fn audio_layer(id: u64, start: i64, asset_id: &str) -> Layer {
    let mut layer =
        Layer::new(LayerId::new(id), format!("audio {id}"), Graph::new()).with_time(start, 0, 300);
    layer.audio = Some(AudioSource::new(asset_id, 0));
    layer
}

/// A project state with a registered stub-backed audio service. Returns
/// both entities plus the recording; the caller keeps the entities alive.
fn init_project_with_audio(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<ProjectState>,
    gpui::Entity<AudioService>,
    Recording,
) {
    ravel_app::project_state::disable_background_eval_for_tests();
    cx.update(|cx| {
        let recording = Recording::default();
        let audio = cx.new(|_| {
            AudioService::with_sink(
                Some(Box::new(StubSink {
                    recording: recording.clone(),
                    clock: SyncClock::new(48_000, FPS),
                })),
                48_000,
            )
        });
        cx.set_global(AudioServiceHandle(audio.downgrade()));
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        (project, audio, recording)
    })
}

fn commit_layer(project: &gpui::Entity<ProjectState>, layer: Layer, cx: &mut TestAppContext) {
    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.expect("root comp");
        let document = ravel_ui::document::add_layer(project.document(), comp, layer).unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
}

// ---------------------------------------------------------------------------
// Document → mixer command diffing
// ---------------------------------------------------------------------------

/// An audio layer on the document becomes one `SetTrack` whose placement is
/// converted to output-rate frames — the fixed "sound comes out" contract.
#[gpui::test]
fn audio_layer_produces_a_set_track(cx: &mut TestAppContext) {
    let (project, audio, recording) = init_project_with_audio(cx);
    audio.update(cx, |service, _| {
        service.cache_decoded(
            CacheKey {
                asset_id: "music".into(),
                stream_index: 0,
            },
            decoded(48_000, 2, 48_000),
        );
    });

    // Layer starts at comp frame 30 = 1s at 30fps.
    commit_layer(&project, audio_layer(1, 30, "music"), cx);

    assert_eq!(
        recording.commands(),
        vec![Recorded::SetTrack {
            id: 1,
            start_frame: 48_000,
            muted: false,
            solo: false,
            sample_rate: 48_000,
        }]
    );
}

/// Moving a layer mid-playback replaces the track atomically (one
/// `SetTrack`, no remove/add gap); an unrelated edit sends nothing.
#[gpui::test]
fn layer_moves_send_minimal_diffs(cx: &mut TestAppContext) {
    let (project, audio, recording) = init_project_with_audio(cx);
    audio.update(cx, |service, _| {
        service.cache_decoded(
            CacheKey {
                asset_id: "music".into(),
                stream_index: 0,
            },
            decoded(48_000, 2, 48_000),
        );
    });
    commit_layer(&project, audio_layer(1, 0, "music"), cx);
    assert_eq!(recording.commands().len(), 1);

    // Move the layer to comp frame 60 (= 2s): one replacing SetTrack.
    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.unwrap();
        let document =
            ravel_ui::document::update_layer(project.document(), comp, LayerId::new(1), |layer| {
                layer.start_frame = 60;
            })
            .unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    let commands = recording.commands();
    assert_eq!(commands.len(), 2, "one replacement, no remove/add gap");
    assert_eq!(
        commands[1],
        Recorded::SetTrack {
            id: 1,
            start_frame: 96_000,
            muted: false,
            solo: false,
            sample_rate: 48_000,
        }
    );

    // An edit that does not touch the audio layer sends nothing new.
    let plain = Layer::new(LayerId::new(2), "solid", Graph::new()).with_time(0, 0, 300);
    commit_layer(&project, plain, cx);
    assert_eq!(recording.commands().len(), 2);
}

/// Removing the layer removes the mixer track.
#[gpui::test]
fn removing_the_layer_removes_the_track(cx: &mut TestAppContext) {
    let (project, audio, recording) = init_project_with_audio(cx);
    audio.update(cx, |service, _| {
        service.cache_decoded(
            CacheKey {
                asset_id: "music".into(),
                stream_index: 0,
            },
            decoded(48_000, 2, 48_000),
        );
    });
    commit_layer(&project, audio_layer(1, 0, "music"), cx);

    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.unwrap();
        let document =
            ravel_ui::document::remove_layer(project.document(), comp, LayerId::new(1)).unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });

    assert_eq!(recording.commands()[1], Recorded::RemoveTrack(1));
}

/// Layer mute and the audio-only mute both silence the track; another
/// layer's solo silences it too (the compositor's `active_layers` rule).
#[gpui::test]
fn mute_and_solo_map_to_the_mixer(cx: &mut TestAppContext) {
    let (project, audio, recording) = init_project_with_audio(cx);
    audio.update(cx, |service, _| {
        service.cache_decoded(
            CacheKey {
                asset_id: "a".into(),
                stream_index: 0,
            },
            decoded(48_000, 2, 48_000),
        );
    });

    commit_layer(&project, audio_layer(1, 0, "a"), cx);
    assert!(matches!(
        recording.commands()[0],
        Recorded::SetTrack { muted: false, .. }
    ));

    // The audio-only mute silences the track.
    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.unwrap();
        let document =
            ravel_ui::document::update_layer(project.document(), comp, LayerId::new(1), |layer| {
                layer.audio.as_mut().unwrap().audio_muted = true
            })
            .unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    assert!(matches!(
        recording.commands()[1],
        Recorded::SetTrack { muted: true, .. }
    ));

    // Unmute again, then solo an unrelated (silent) layer: every audio
    // track is silenced by the compositor's `active_layers` rule.
    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.unwrap();
        let document =
            ravel_ui::document::update_layer(project.document(), comp, LayerId::new(1), |layer| {
                layer.audio.as_mut().unwrap().audio_muted = false
            })
            .unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.unwrap();
        let mut soloed = Layer::new(LayerId::new(2), "video", Graph::new()).with_time(0, 0, 300);
        soloed.solo = true;
        let document = ravel_ui::document::add_layer(project.document(), comp, soloed).unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    assert!(matches!(
        recording.commands()[3],
        Recorded::SetTrack {
            id: 1,
            muted: true,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Clock switch
// ---------------------------------------------------------------------------

/// The single clock-switch decision: no audio tracks ⇒ no audio clock
/// (wall fallback); audio tracks + a running sink ⇒ the sync clock.
#[gpui::test]
fn clock_switches_only_with_tracks_and_engine(cx: &mut TestAppContext) {
    let (project, audio, recording) = init_project_with_audio(cx);
    let _ = recording;
    assert!(
        audio
            .read_with(cx, |service, _| service.audio_clock())
            .is_none()
    );
    cx.update(|cx| assert!(ravel_app::audio::playback_clock(cx).is_none()));

    audio.update(cx, |service, _| {
        service.cache_decoded(
            CacheKey {
                asset_id: "music".into(),
                stream_index: 0,
            },
            decoded(48_000, 2, 48_000),
        );
    });
    commit_layer(&project, audio_layer(1, 0, "music"), cx);

    let clock = audio.read_with(cx, |service, _| service.audio_clock());
    assert!(clock.is_some(), "audio tracks + engine ⇒ audio clock");
    cx.update(|cx| assert!(ravel_app::audio::playback_clock(cx).is_some()));

    // Removing the layer falls back to the wall clock.
    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.unwrap();
        let document =
            ravel_ui::document::remove_layer(project.document(), comp, LayerId::new(1)).unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    assert!(
        audio
            .read_with(cx, |service, _| service.audio_clock())
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Transport under the audio clock
// ---------------------------------------------------------------------------

#[test]
fn tick_follows_the_audio_clock() {
    let sync = SyncClock::new(48_000, FPS);
    sync.set_playing(true);
    let mut transport = Transport::new(FPS, 300);
    let t0 = Instant::now();
    transport.toggle(t0);

    // The device played 1s of samples ⇒ frame 30, however the wall clock
    // (here: no time passed at all) disagrees.
    sync.seek_to_sample(48_000);
    let update = transport
        .tick_with(&ClockSource::Audio(&sync))
        .expect("frame moved");
    assert_eq!(update.frame, 30);
    assert!(update.playing);
}

#[test]
fn audio_clock_auto_pauses_at_the_timeline_end() {
    let sync = SyncClock::new(48_000, FPS);
    sync.set_playing(true);
    let mut transport = Transport::new(FPS, 300);
    transport.toggle(Instant::now());

    // 20s of samples with a 10s timeline: hold the last frame and pause.
    sync.seek_to_sample(48_000 * 20);
    let update = transport
        .tick_with(&ClockSource::Audio(&sync))
        .expect("frame moved");
    assert_eq!(update.frame, 299);
    assert!(!update.playing);
    assert_eq!(transport.state(), PlaybackState::Paused);
}

#[test]
fn pausing_on_the_audio_clock_freezes_the_audio_position() {
    let sync = SyncClock::new(48_000, FPS);
    sync.set_playing(true);
    let t0 = Instant::now();
    let mut transport = Transport::new(FPS, 300);
    transport.toggle(t0);

    sync.seek_to_sample(48_000 + 16_000); // 1.33s ⇒ frame 40
    let update = transport.toggle_with(&ClockSource::Audio(&sync), t0 + Duration::from_millis(1));
    assert_eq!(update.frame, 40);
    assert!(!update.playing);
    // The frozen position survives wall-clock reads (step/seek anchoring).
    assert_eq!(transport.current_frame(), 40);
}

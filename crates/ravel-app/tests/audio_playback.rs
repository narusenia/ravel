// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Audio playback wiring tests
//! (`docs/implementation/audio-plan.md`, units 3 and 4).
//!
//! The real CPAL engine cannot run in CI (no output device), so these tests
//! drive [`AudioService`] with a recording stub sink and verify:
//!
//! - an audio layer on the document produces a `SetTrack` command whose
//!   content matches the layer (the "sound comes out" path, fixed at the
//!   command boundary),
//! - mid-playback edits send minimal diffs (`SetTrack` replacement, never a
//!   remove/add gap),
//! - switching a layer's audio stream sends the other stream's track (unit 4),
//! - picture and sound read the same layer-local time axis, and the shell's
//!   trim bounds both (unit 4),
//! - the playback clock switch: audio clock with tracks + engine, wall
//!   clock otherwise.

use core::prelude::v1::test;

use gpui::{AppContext as _, TestAppContext};
use ravel_app::audio::{AudioService, AudioServiceHandle, AudioSink};
use ravel_app::playback::{ClockSource, Transport};
use ravel_app::project_state::{ProjectState, ProjectStateHandle};
use ravel_audio::mixdown::{AudioMixdown, CacheKey, DecodedAudio};
use ravel_audio::{AudioCommand, AudioError, SyncClock};
use ravel_core::composition::{AudioSource, Composition, Layer};
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId};
use ravel_core::media::VideoStreamInfo;
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
        channels: u32,
        muted: bool,
        solo: bool,
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
            AudioCommand::SetTrack(track) => Recorded::SetTrack {
                id: track.id,
                start_frame: track.start_frame,
                channels: track.channels,
                muted: track.muted,
                solo: track.solo,
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
            channels: 2,
            muted: false,
            solo: false,
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
            channels: 2,
            muted: false,
            solo: false,
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
// Video layer audio (audio-plan unit 4)
// ---------------------------------------------------------------------------

/// Switching the audio stream of a layer sends a new `SetTrack` built from
/// the other stream — the sound that plays actually changes. The two streams
/// use different channel counts, while both cached buffers obey the output-rate
/// invariant.
#[gpui::test]
fn switching_the_stream_sends_the_other_streams_track(cx: &mut TestAppContext) {
    let (project, audio, recording) = init_project_with_audio(cx);
    audio.update(cx, |service, _| {
        service.cache_decoded(
            CacheKey {
                asset_id: "clip".into(),
                stream_index: 1,
            },
            decoded(48_000, 2, 48_000),
        );
        service.cache_decoded(
            CacheKey {
                asset_id: "clip".into(),
                stream_index: 2,
            },
            decoded(48_000, 1, 48_000),
        );
    });

    let mut layer = Layer::new(LayerId::new(1), "clip", Graph::new()).with_time(0, 0, 300);
    layer.audio = Some(AudioSource::new("clip", 1));
    commit_layer(&project, layer, cx);
    assert_eq!(
        recording.commands(),
        vec![Recorded::SetTrack {
            id: 1,
            start_frame: 0,
            channels: 2,
            muted: false,
            solo: false,
        }]
    );

    // The Properties stream picker writes `stream_index`; the mixdown's build
    // key includes it, so the track is rebuilt from the new stream.
    project.update(cx, |project, cx| {
        let comp = project.document().root_comp.unwrap();
        let document =
            ravel_ui::document::update_layer(project.document(), comp, LayerId::new(1), |layer| {
                layer.audio.as_mut().unwrap().stream_index = 2;
            })
            .unwrap();
        project.commit_document(document, InvalidationHint::Structural, cx);
    });
    let commands = recording.commands();
    assert_eq!(commands.len(), 2, "one replacing SetTrack, no gap");
    assert_eq!(
        commands[1],
        Recorded::SetTrack {
            id: 1,
            start_frame: 0,
            channels: 1,
            muted: false,
            solo: false,
        },
        "the second stream's audio is what now plays"
    );
}

/// Picture and sound read the same layer-local time axis.
///
/// The `media` node maps layer-local **seconds** onto a source frame, while
/// the mixer places the track by output-rate sample frames. This pins both
/// against the same composition frame: the second of source the audio is
/// playing must be the second of source the picture is showing, including
/// with a trimmed `in_frame` and a media rate that differs from the
/// composition rate.
#[test]
fn picture_and_sound_share_the_layer_local_time_axis() {
    const OUTPUT_RATE: u32 = 48_000;
    let media_stream = VideoStreamInfo {
        stream_index: 0,
        codec: None,
        codec_name: "fake".into(),
        width: 640,
        height: 480,
        frame_rate: FrameRate::new(24, 1), // deliberately not the comp rate
        frame_count: None,
        duration_secs: None,
        pixel_format: "rgba".into(),
        color_primaries: None,
        color_transfer: None,
        color_matrix: None,
    };

    // A clip placed at comp frame 30, trimmed to source frames 10..100.
    let mut layer = Layer::new(LayerId::new(1), "clip", Graph::new()).with_time(30, 10, 100);
    layer.audio = Some(AudioSource::new("clip", 1));
    let mut comp = Composition::new(CompId::new(1), "comp", (640, 480), FPS, 300);
    comp.layers = vec![layer.clone()].into();

    let spec = &AudioMixdown::desired_tracks(&comp, OUTPUT_RATE)[0];
    assert_eq!(spec.source_in_frames, 10);
    assert_eq!(spec.source_out_frames, 100);

    for comp_frame in [30u64, 45, 99] {
        // Picture: the network boundary's layer-local frame in seconds
        // (`comp_frame - start + in`), mapped onto the media's own rate.
        let local_frame = layer.local_frame(comp_frame);
        let local_secs = local_frame as f64 / FPS.as_f64();
        let video_frame = ravel_nodes::media::media_frame_for(local_secs, &media_stream);
        let video_secs = video_frame as f64 / media_stream.frame_rate.as_f64();

        // Sound: where in the mixed timeline this comp frame lands, minus the
        // track's start, is the offset into the track — whose first sample is
        // source frame `source_in_frames`.
        let timeline_sample = comp_frame * OUTPUT_RATE as u64 / FPS.num as u64;
        let offset_samples = timeline_sample - spec.start_frame;
        let audio_secs = spec.source_in_frames as f64 / FPS.as_f64()
            + offset_samples as f64 / OUTPUT_RATE as f64;

        // Both agree to within one media frame (the picture is quantised to
        // the media's frame grid; the audio is not).
        let tolerance = 1.0 / media_stream.frame_rate.as_f64();
        assert!(
            (audio_secs - video_secs).abs() < tolerance,
            "comp frame {comp_frame}: picture at {video_secs}s, sound at {audio_secs}s"
        );
        assert!(
            (audio_secs - local_secs).abs() < 1e-9,
            "comp frame {comp_frame}: sound must sit exactly on the layer-local axis"
        );
    }
}

/// The shell's `out_frame` trims the sound as well as the picture: the built
/// track ends where the layer stops showing frames.
#[test]
fn the_layer_trim_bounds_the_audible_range() {
    const OUTPUT_RATE: u32 = 48_000;
    let mut layer = Layer::new(LayerId::new(1), "clip", Graph::new()).with_time(30, 10, 100);
    layer.audio = Some(AudioSource::new("clip", 1));
    let mut comp = Composition::new(CompId::new(1), "comp", (640, 480), FPS, 300);
    comp.layers = vec![layer].into();

    let spec = &AudioMixdown::desired_tracks(&comp, OUTPUT_RATE)[0];
    // 10 s of source at 48 kHz; the layer only plays comp frames 10..100,
    // i.e. source seconds 1/3 .. 10/3.
    let track = AudioMixdown::build_track(spec, &decoded(10 * 48_000, 2, 48_000), FPS, OUTPUT_RATE)
        .expect("audible range");
    assert_eq!(track.frame_count(), (90 * 48_000) / 30, "90 comp frames");
    assert_eq!(track.start_frame, 30 * 48_000 / 30);
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
fn audio_clock_reports_auto_pause_after_the_last_frame_was_published() {
    let sync = SyncClock::new(48_000, FPS);
    sync.set_playing(true);
    let mut transport = Transport::new(FPS, 300);
    transport.toggle(Instant::now());

    sync.seek_to_sample(299 * 1_600);
    let last = transport
        .tick_with(&ClockSource::Audio(&sync))
        .expect("last frame is published");
    assert_eq!(last.frame, 299);
    assert!(last.playing);

    sync.seek_to_sample(300 * 1_600);
    let paused = transport
        .tick_with(&ClockSource::Audio(&sync))
        .expect("the state change must be published");
    assert_eq!(paused.frame, 299);
    assert!(!paused.playing);
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

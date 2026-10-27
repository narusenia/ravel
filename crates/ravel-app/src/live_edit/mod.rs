// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The local live-edit door: a Unix socket (macOS / Linux) or a named pipe
//! (Windows) an agent on the same machine uses to edit the open document.
//!
//! **Closed by default.** It opens only when the *global* settings layer says
//! `[agent] live_edit_enabled = true`, read once at startup. The project layer
//! is never consulted: a `.ravprj` comes from elsewhere and must not be able
//! to open a door into the user's session.
//!
//! Threads: accepting and reading run on their own threads, so the UI thread
//! never blocks on I/O. Each request line crosses to the main thread over a
//! `futures` channel (the shape of `media::watch::AssetWatch`), where
//! [`LiveEdit`] applies it through [`ProjectState::commit_edit_ops`] — one
//! request, one commit, one undo step — and answers on a per-request channel.
//! Nothing here runs from `render()` and nothing is parked in a `Global`.
//!
//! The wire format is in [`protocol`]; `docs/dev/live-edit.md` is the guide.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;

use gpui::{App, AppContext as _, Context, Entity, Task};

use crate::app_settings::{AppSettings, SettingsScope};
use crate::project_state::ProjectState;

pub mod protocol;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// A parsed request on its way to the main thread, with the way back.
pub struct Job {
    pub request: protocol::Request,
    pub reply: mpsc::SyncSender<String>,
}

type JobSender = futures::channel::mpsc::UnboundedSender<Job>;

/// Whether the global settings layer opens the door.
fn enabled_in_global_layer(cx: &App) -> bool {
    cx.try_global::<AppSettings>().is_some_and(|settings| {
        settings
            .layer(SettingsScope::Global)
            .agent
            .live_edit_enabled
            .unwrap_or(false)
    })
}

/// Owner of the open door. Dropping it stops the drain and removes the socket
/// file; the listener thread ends with the process.
pub struct LiveEdit {
    #[cfg(unix)]
    path: PathBuf,
    _drain: Task<()>,
}

/// Open the door if the global settings say so. `None` when closed or when it
/// could not be opened (logged).
pub fn start(project: &Entity<ProjectState>, cx: &mut App) -> Option<Entity<LiveEdit>> {
    start_at(project, enabled_in_global_layer(cx), None, cx)
}

/// [`start`] with the switch and the endpoint given (tests point the socket at
/// a temporary directory).
pub fn start_at(
    project: &Entity<ProjectState>,
    enabled: bool,
    endpoint: Option<PathBuf>,
    cx: &mut App,
) -> Option<Entity<LiveEdit>> {
    if !enabled {
        return None;
    }
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Job>();
    #[cfg(unix)]
    let path = unix::open(endpoint, tx)?;
    #[cfg(windows)]
    windows::open(endpoint, tx)?;
    let project = project.downgrade();
    let entity = cx.new(|cx: &mut Context<LiveEdit>| LiveEdit {
        #[cfg(unix)]
        path: path.clone(),
        _drain: cx.spawn(async move |_this, cx| {
            use futures::StreamExt as _;
            while let Some(job) = rx.next().await {
                let Some(project) = project.upgrade() else {
                    break;
                };
                let reply = cx.update(|cx| project.update(cx, |p, cx| apply(p, &job.request, cx)));
                let _ = job.reply.send(reply);
            }
        }),
    });
    #[cfg(unix)]
    cx.on_app_quit(move |_| {
        let _ = std::fs::remove_file(&path);
        async {}
    })
    .detach();
    Some(entity)
}

#[cfg(unix)]
impl Drop for LiveEdit {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Apply one request on the main thread; the response line.
pub fn apply(
    project: &mut ProjectState,
    request: &protocol::Request,
    cx: &mut Context<ProjectState>,
) -> String {
    if let Some(index) = protocol::first_removal(&request.ops) {
        return protocol::confirmation_required(&request.id, index);
    }
    match project.commit_edit_ops(&request.ops, cx) {
        Ok(applied) => protocol::ok_response(&request.id, &applied),
        Err(err) => protocol::batch_error_response(&request.id, &err),
    }
}

/// Serve one connection until it closes: a line in, a line out. Runs on the
/// connection's own thread.
fn serve_connection<S: Read + Write>(stream: S, tx: JobSender) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut buf = Vec::new();
        let read = (&mut reader)
            .take(protocol::MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf);
        if !matches!(read, Ok(n) if n > 0) {
            return;
        }
        let too_long = buf.len() > protocol::MAX_LINE_BYTES;
        let reply = match std::str::from_utf8(&buf) {
            _ if too_long => protocol::bad_request("the request line exceeds 4 MiB"),
            Err(_) => protocol::bad_request("the request line is not UTF-8"),
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => match protocol::parse_request(line) {
                Err(refusal) => refusal,
                Ok(request) => {
                    let (reply, answer) = mpsc::sync_channel(1);
                    if tx.unbounded_send(Job { request, reply }).is_err() {
                        return;
                    }
                    match answer.recv() {
                        Ok(line) => line,
                        Err(_) => return,
                    }
                }
            },
        };
        let stream = reader.get_mut();
        if writeln!(stream, "{reply}")
            .and_then(|()| stream.flush())
            .is_err()
            || too_long
        {
            return;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::project_state::disable_background_eval_for_tests;
    use gpui::TestAppContext;
    use ravel_core::edit::{CompositionSettings, EditOp};
    use serde_json::Value;
    use std::os::unix::fs::PermissionsExt as _;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// One connection's bytes: what the client sent, and what came back.
    struct Duplex {
        input: std::io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn served(input: Vec<u8>) -> Vec<Value> {
        let (tx, _rx) = futures::channel::mpsc::unbounded::<Job>();
        let mut duplex = Duplex {
            input: std::io::Cursor::new(input),
            output: Vec::new(),
        };
        serve_connection(&mut duplex, tx);
        String::from_utf8(duplex.output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn an_unreadable_line_is_refused_with_its_reason() {
        let replies = served(b"\xff\xfe\n".to_vec());
        assert_eq!(replies[0]["error"]["code"], "bad-request");
        assert_eq!(
            replies[0]["error"]["message"],
            "the request line is not UTF-8"
        );

        let mut long = vec![b' '; protocol::MAX_LINE_BYTES + 1];
        long.extend_from_slice(b"\n{}\n");
        let replies = served(long);
        assert_eq!(
            replies.len(),
            1,
            "the connection closes after an oversize line"
        );
        assert_eq!(
            replies[0]["error"]["message"],
            "the request line exceeds 4 MiB"
        );
    }

    #[gpui::test]
    fn a_disabled_door_leaves_no_socket(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ravel").join("live-edit.sock");
        let project = cx.new(ProjectState::new);
        // Unset settings (no `AppSettings` global at all) and an explicit
        // `false` both leave the door shut.
        assert!(cx.update(|cx| !enabled_in_global_layer(cx)));
        assert!(cx.update(|cx| start_at(&project, false, Some(path.clone()), cx).is_none()));
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[gpui::test]
    fn the_project_layer_cannot_open_the_door(cx: &mut TestAppContext) {
        use ravel_project::settings::{AgentLayer, SettingsLayer};
        cx.update(|cx| {
            crate::app_settings::install(Default::default(), cx);
            crate::app_settings::set_project_layer(
                SettingsLayer {
                    agent: AgentLayer {
                        live_edit_enabled: Some(true),
                    },
                    ..Default::default()
                },
                cx,
            );
            assert!(!enabled_in_global_layer(cx));
            // The control: the same predicate opens for the global layer, so
            // the refusal above is not a predicate that always says no.
            crate::app_settings::update(
                SettingsScope::Global,
                |layer| layer.agent.live_edit_enabled = Some(true),
                cx,
            );
            assert!(enabled_in_global_layer(cx));
        });
    }

    #[gpui::test]
    fn the_socket_and_its_directory_belong_to_the_user_alone(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ravel").join("live-edit.sock");
        let project = cx.new(ProjectState::new);
        let door = cx.update(|cx| start_at(&project, true, Some(path.clone()), cx));
        assert!(door.is_some());
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        // A second instance finds the first one alive and leaves it be.
        assert!(cx.update(|cx| start_at(&project, true, Some(path.clone()), cx).is_none()));
        assert!(path.exists());
        drop(door);
        cx.update(|_| {});
        assert!(!path.exists(), "the socket outlived its owner");
    }

    /// One request line goes the whole way — protocol, main-thread apply,
    /// `commit_edit_ops` — and costs exactly one undo step.
    #[gpui::test]
    fn one_request_is_one_undo_step(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let original = project.read_with(cx, |p, _| p.document().clone());
        let ops = [
            EditOp::AddComposition {
                settings: CompositionSettings::fallback("Live"),
                alias: Some("c".into()),
            },
            EditOp::AddLayer {
                comp: ravel_core::edit::CompRef::Alias("c".into()),
                template: "solid".into(),
                alias: None,
            },
        ];
        let line = serde_json::json!({"id": "r1", "ops": ops}).to_string();
        let request = protocol::parse_request(&line).expect("well-formed");
        let reply = project.update(cx, |p, cx| apply(p, &request, cx));
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["id"], "r1");
        assert_eq!(reply["ok"]["created"].as_array().unwrap().len(), 2);
        project.update(cx, |p, cx| {
            assert_ne!(*p.document(), original);
            assert!(p.undo(cx), "no undo step");
            assert_eq!(*p.document(), original);
            assert!(!p.undo(cx), "more than one undo step");
        });
    }

    #[gpui::test]
    fn a_request_that_removes_is_refused_whole(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let original = project.read_with(cx, |p, _| p.document().clone());
        let comp = original.compositions.keys().next().copied().unwrap();
        let ops = [
            EditOp::AddComposition {
                settings: CompositionSettings::fallback("Live"),
                alias: None,
            },
            EditOp::RemoveComposition {
                comp: ravel_core::edit::CompRef::Id(comp),
            },
        ];
        let line = serde_json::json!({"id": 1, "ops": ops}).to_string();
        let request = protocol::parse_request(&line).unwrap();
        let reply = project.update(cx, |p, cx| apply(p, &request, cx));
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["error"]["code"], "confirmation-required");
        assert_eq!(reply["error"]["index"], 1);
        project.update(cx, |p, cx| {
            assert_eq!(*p.document(), original);
            assert!(!p.undo(cx), "a refusal left an undo step");
        });
    }

    #[gpui::test]
    fn an_empty_request_succeeds_without_an_undo_step(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let request = protocol::parse_request(r#"{"id": null, "ops": []}"#).unwrap();
        let reply = project.update(cx, |p, cx| apply(p, &request, cx));
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert!(reply["ok"]["created"].as_array().unwrap().is_empty());
        project.update(cx, |p, cx| assert!(!p.undo(cx)));
    }

    /// The Properties scrub may collapse an expression row to a constant; a
    /// request from outside may not.
    #[gpui::test]
    fn a_request_cannot_overwrite_a_driven_parameter(cx: &mut TestAppContext) {
        use ravel_core::animation::channel::{
            AnimationChannel, ChannelSource, ParameterExpression,
        };
        use ravel_core::edit::{Created, NetworkPath, NodeRef, ParamInput};
        use ravel_core::graph::{Parameter, ParameterValue};
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let setup = [
            EditOp::AddComposition {
                settings: CompositionSettings::fallback("Live"),
                alias: Some("c".into()),
            },
            EditOp::AddLayer {
                comp: ravel_core::edit::CompRef::Alias("c".into()),
                template: "solid".into(),
                alias: Some("l".into()),
            },
        ];
        let created = project
            .update(cx, |p, cx| p.commit_edit_ops(&setup, cx))
            .unwrap()
            .created;
        let (comp, layer) = match created[..] {
            [Created::Composition(c), Created::Layer(l)] => (c, l),
            _ => panic!("unexpected created {created:?}"),
        };
        let path = NetworkPath::layer(comp, layer);
        let add = [EditOp::AddNode {
            path: path.clone().into(),
            type_key: "style.stroke".into(),
            position: None,
            params: Default::default(),
            alias: None,
        }];
        let node = project
            .update(cx, |p, cx| p.commit_edit_ops(&add, cx))
            .unwrap()
            .created
            .iter()
            .find_map(|c| match c {
                Created::Node(id) => Some(*id),
                _ => None,
            })
            .unwrap();
        project.update(cx, |p, cx| {
            let doc = p.document().clone();
            let graph = ravel_core::edit::document::resolve_network(&doc, &path)
                .unwrap()
                .clone()
                .set_params(
                    node,
                    &[Parameter {
                        key: "width".into(),
                        value: ParameterValue::Channel(AnimationChannel::new(
                            ChannelSource::Expression(ParameterExpression::new("frame")),
                        )),
                    }],
                )
                .unwrap();
            let doc = ravel_core::edit::document::replace_network(&doc, &path, graph).unwrap();
            p.commit_document(doc, ravel_core::runtime::InvalidationHint::Structural, cx);
        });
        let before = project.read_with(cx, |p, _| p.document().clone());
        let ops = [EditOp::SetParameter {
            path: path.into(),
            node: NodeRef::Id(node),
            key: "width".into(),
            value: ParamInput::Float(4.0),
        }];
        let line = serde_json::json!({"id": 1, "ops": ops}).to_string();
        let request = protocol::parse_request(&line).unwrap();
        let reply = project.update(cx, |p, cx| apply(p, &request, cx));
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["error"]["code"], "driven-parameter");
        project.read_with(cx, |p, _| assert_eq!(*p.document(), before));
    }
}

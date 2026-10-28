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
//! A request that removes something waits for the user instead
//! ([`confirm`]): [`LiveEdit`] emits [`LiveEditEvent::ConfirmRemoval`], the
//! workspace (which owns the window) subscribes and opens the dialog, and the
//! drain task awaits the verdict. Requests are handled one at a time, so a
//! request from another connection simply queues behind the open question.
//!
//! The wire format is in [`protocol`]; `docs/dev/live-edit.md` is the guide.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;

use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Task, WeakEntity};

use crate::app_settings::{AppSettings, SettingsScope};
use crate::project_state::ProjectState;

pub mod confirm;
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

/// What the door asks the window-owning side to do.
pub enum LiveEditEvent {
    /// Ask the user whether `request.items` may be removed. Not answering is
    /// answering no.
    ConfirmRemoval(confirm::RemovalRequest),
}

impl EventEmitter<LiveEditEvent> for LiveEdit {}

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
    let (tx, rx) = futures::channel::mpsc::unbounded::<Job>();
    #[cfg(unix)]
    let path = unix::open(endpoint, tx)?;
    #[cfg(windows)]
    windows::open(endpoint, tx)?;
    let entity = cx.new(|cx: &mut Context<LiveEdit>| {
        LiveEdit::new(
            project,
            rx,
            #[cfg(unix)]
            path.clone(),
            cx,
        )
    });
    #[cfg(unix)]
    cx.on_app_quit(move |_| {
        let _ = std::fs::remove_file(&path);
        async {}
    })
    .detach();
    Some(entity)
}

impl LiveEdit {
    fn new(
        project: &Entity<ProjectState>,
        mut rx: futures::channel::mpsc::UnboundedReceiver<Job>,
        #[cfg(unix)] path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = project.downgrade();
        Self {
            #[cfg(unix)]
            path,
            _drain: cx.spawn(async move |this, cx| {
                use futures::StreamExt as _;
                while let Some(job) = rx.next().await {
                    let Some(project) = project.upgrade() else {
                        break;
                    };
                    let Some(reply) = handle(&project, &this, &job.request, cx).await else {
                        break;
                    };
                    let _ = job.reply.send(reply);
                }
            }),
        }
    }
}

/// One request, from the line to the response line. `None` when the door or
/// the project is gone.
async fn handle(
    project: &Entity<ProjectState>,
    door: &WeakEntity<LiveEdit>,
    request: &protocol::Request,
    cx: &mut AsyncApp,
) -> Option<String> {
    let Some(index) = protocol::first_removal(&request.ops) else {
        return Some(cx.update(|cx| project.update(cx, |p, cx| apply(p, request, cx))));
    };
    // What the user is shown is this document; the batch is applied to this
    // document or to nothing.
    let (asked, verdict) = cx.update(|cx| {
        let p = project.read(cx);
        // The generation tells a reopened project apart from this one even
        // when the two documents are equal.
        let doc = (p.generation(), p.document().clone());
        let (ask, verdict) =
            confirm::RemovalRequest::new(confirm::describe_removals(&doc.1, &request.ops));
        // With nobody subscribed the event is dropped with `ask`, which reads
        // as a denial.
        door.update(cx, |_, cx| cx.emit(LiveEditEvent::ConfirmRemoval(ask)))
            .ok()?;
        Some((doc, verdict))
    })?;
    if !verdict.await.unwrap_or(false) {
        return Some(protocol::confirmation_denied(&request.id, index));
    }
    Some(cx.update(|cx| {
        project.update(cx, |p, cx| {
            if (p.generation(), p.document()) == (asked.0, &asked.1) {
                apply(p, request, cx)
            } else {
                protocol::document_changed(&request.id)
            }
        })
    }))
}

#[cfg(unix)]
impl Drop for LiveEdit {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Commit one request on the main thread; the response line. A request that
/// removes something must have been confirmed before it gets here
/// ([`handle`] is the only caller that lets one through).
fn apply(
    project: &mut ProjectState,
    request: &protocol::Request,
    cx: &mut Context<ProjectState>,
) -> String {
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

    /// A door with no socket: jobs are pushed by the test, and the
    /// confirmation questions it emits are collected.
    struct Harness {
        project: Entity<ProjectState>,
        tx: futures::channel::mpsc::UnboundedSender<Job>,
        asked: std::rc::Rc<std::cell::RefCell<Vec<confirm::RemovalRequest>>>,
        _door: Entity<LiveEdit>,
        _sub: gpui::Subscription,
    }

    impl Harness {
        fn new(cx: &mut TestAppContext) -> Self {
            disable_background_eval_for_tests();
            let project = cx.new(ProjectState::new);
            let (tx, rx) = futures::channel::mpsc::unbounded::<Job>();
            let door = cx.update(|cx| {
                cx.new(|cx| LiveEdit::new(&project, rx, std::path::PathBuf::new(), cx))
            });
            let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let sink = asked.clone();
            let _sub = cx.update(|cx| {
                cx.subscribe(&door, move |_, event: &LiveEditEvent, _| {
                    let LiveEditEvent::ConfirmRemoval(request) = event;
                    sink.borrow_mut().push(request.clone());
                })
            });
            Self {
                project,
                tx,
                asked,
                _door: door,
                _sub,
            }
        }

        fn send(&self, id: i64, ops: &[EditOp]) -> mpsc::Receiver<String> {
            let line = serde_json::json!({"id": id, "ops": ops}).to_string();
            let request = protocol::parse_request(&line).unwrap();
            let (reply, answer) = mpsc::sync_channel(1);
            self.tx.unbounded_send(Job { request, reply }).unwrap();
            answer
        }

        fn document(&self, cx: &TestAppContext) -> ravel_core::composition::Document {
            self.project.read_with(cx, |p, _| p.document().clone())
        }

        fn undo_steps(&self, cx: &mut TestAppContext) -> usize {
            self.project.update(cx, |p, cx| {
                let mut steps = 0;
                while p.undo(cx) {
                    steps += 1;
                }
                steps
            })
        }
    }

    fn remove_first_comp(doc: &ravel_core::composition::Document) -> [EditOp; 1] {
        [EditOp::RemoveComposition {
            comp: ravel_core::edit::CompRef::Id(doc.compositions.keys().next().copied().unwrap()),
        }]
    }

    fn reply_of(answer: &mpsc::Receiver<String>) -> Value {
        serde_json::from_str(&answer.try_recv().expect("no reply yet")).unwrap()
    }

    #[gpui::test]
    fn a_removal_waits_and_an_approval_applies_it_as_one_undo_step(cx: &mut TestAppContext) {
        let h = Harness::new(cx);
        // Two compositions, so a removal leaves a document that differs.
        let add = [EditOp::AddComposition {
            settings: CompositionSettings::fallback("Second"),
            alias: None,
        }];
        h.project
            .update(cx, |p, cx| p.commit_edit_ops(&add, cx))
            .unwrap();
        let before = h.document(cx);
        let answer = h.send(1, &remove_first_comp(&before));
        cx.run_until_parked();
        assert_eq!(h.asked.borrow().len(), 1, "the user was not asked");
        assert_eq!(
            h.asked.borrow()[0].items,
            [confirm::Removal::Composition {
                name: before.compositions.values().next().unwrap().name.clone()
            }]
        );
        assert!(answer.try_recv().is_err(), "answered before the user did");
        assert_eq!(h.document(cx), before, "applied without approval");

        h.asked.borrow()[0].approve();
        cx.run_until_parked();
        let reply = reply_of(&answer);
        assert!(reply["ok"].is_object(), "{reply}");
        assert_ne!(h.document(cx), before);
        // The setup commit plus the approved removal: two steps, not three.
        assert_eq!(h.undo_steps(cx), 2);
    }

    #[gpui::test]
    fn a_denial_leaves_the_document_and_its_history_alone(cx: &mut TestAppContext) {
        let h = Harness::new(cx);
        let before = h.document(cx);
        let answer = h.send(7, &remove_first_comp(&before));
        cx.run_until_parked();
        h.asked.borrow()[0].deny();
        cx.run_until_parked();
        let reply = reply_of(&answer);
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["error"]["code"], "confirmation-denied");
        assert_eq!(h.document(cx), before);
        assert_eq!(h.undo_steps(cx), 0);
    }

    /// Escape and the close button never call a handler: they drop the dialog
    /// and with it the request.
    #[gpui::test]
    fn a_dialog_closed_unanswered_is_a_denial(cx: &mut TestAppContext) {
        let h = Harness::new(cx);
        let before = h.document(cx);
        let answer = h.send(1, &remove_first_comp(&before));
        cx.run_until_parked();
        h.asked.borrow_mut().clear();
        cx.run_until_parked();
        assert_eq!(reply_of(&answer)["error"]["code"], "confirmation-denied");
        assert_eq!(h.document(cx), before);
        assert_eq!(h.undo_steps(cx), 0);
    }

    #[gpui::test]
    fn nobody_listening_is_a_denial(cx: &mut TestAppContext) {
        let mut h = Harness::new(cx);
        h._sub = cx.update(|cx| cx.subscribe(&h._door, |_, _: &LiveEditEvent, _| {}));
        let before = h.document(cx);
        let answer = h.send(1, &remove_first_comp(&before));
        cx.run_until_parked();
        assert_eq!(reply_of(&answer)["error"]["code"], "confirmation-denied");
        assert_eq!(h.document(cx), before);
    }

    #[gpui::test]
    fn an_approval_after_the_document_changed_applies_nothing(cx: &mut TestAppContext) {
        let h = Harness::new(cx);
        let before = h.document(cx);
        let answer = h.send(1, &remove_first_comp(&before));
        cx.run_until_parked();
        let edit = [EditOp::AddComposition {
            settings: CompositionSettings::fallback("Typed by the user"),
            alias: None,
        }];
        h.project
            .update(cx, |p, cx| p.commit_edit_ops(&edit, cx))
            .unwrap();
        let edited = h.document(cx);
        h.asked.borrow()[0].approve();
        cx.run_until_parked();
        assert_eq!(reply_of(&answer)["error"]["code"], "document-changed");
        assert_eq!(h.document(cx), edited);
    }

    #[gpui::test]
    fn an_approval_after_the_project_was_replaced_applies_nothing(cx: &mut TestAppContext) {
        let h = Harness::new(cx);
        h.project.update(cx, |p, cx| p.new_document(cx));
        let before = h.document(cx);
        let answer = h.send(1, &remove_first_comp(&before));
        cx.run_until_parked();
        h.project.update(cx, |p, cx| p.reopen_same_document(cx));
        let replaced = h.document(cx);
        assert_eq!(
            replaced, before,
            "precondition: an equal document, a new project"
        );
        h.asked.borrow()[0].approve();
        cx.run_until_parked();
        assert_eq!(reply_of(&answer)["error"]["code"], "document-changed");
        assert_eq!(h.document(cx), replaced);
    }

    #[gpui::test]
    fn a_second_request_queues_behind_the_open_question(cx: &mut TestAppContext) {
        let h = Harness::new(cx);
        let before = h.document(cx);
        let removal = h.send(1, &remove_first_comp(&before));
        let add = [EditOp::AddComposition {
            settings: CompositionSettings::fallback("Other"),
            alias: None,
        }];
        let other = h.send(2, &add);
        cx.run_until_parked();
        assert!(other.try_recv().is_err(), "overtook the open question");
        h.asked.borrow()[0].deny();
        cx.run_until_parked();
        assert_eq!(reply_of(&removal)["error"]["code"], "confirmation-denied");
        assert!(reply_of(&other)["ok"].is_object());
    }

    #[gpui::test]
    fn a_disconnect_is_applied_without_asking(cx: &mut TestAppContext) {
        let h = Harness::new(cx);
        let before = h.document(cx);
        let comp = before.compositions.keys().next().copied().unwrap();
        let layer = ravel_core::id::LayerId::new(1);
        let op = [EditOp::Disconnect {
            path: ravel_core::edit::NetworkPath::layer(comp, layer).into(),
            edge: ravel_core::id::EdgeId::new(999),
        }];
        let answer = h.send(1, &op);
        cx.run_until_parked();
        assert!(h.asked.borrow().is_empty(), "a disconnect was asked about");
        // The edge does not exist: the batch fails on its own terms, which is
        // the proof it reached the commit rather than the dialog.
        assert!(reply_of(&answer)["error"]["code"].is_string());
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

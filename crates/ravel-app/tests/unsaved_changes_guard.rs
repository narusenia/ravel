// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI integration coverage for the destructive project-action guard.

use gpui::{
    AnyWindowHandle, App, AppContext as _, Entity, Keystroke, Modifiers, TestAppContext,
    VisualTestContext, WindowHandle, px, size,
};
use gpui_component::{Root, WindowExt as _};
use ravel_app::panels;
use ravel_app::project_state::ProjectState;
use ravel_app::trace;
use ravel_app::workspace::{self, MainWorkspace, RavelWorkspace};
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;

struct WorkspaceHarness {
    window: WindowHandle<Root>,
    workspace: Entity<RavelWorkspace>,
}

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

fn open_workspace(cx: &mut TestAppContext) -> WorkspaceHarness {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        ravel_app::project_state::disable_background_eval_for_tests();
        cx.set_global(panels::FocusedPanelGlobal(None));
        cx.set_global(panels::SelectedPropertiesTarget::default());
        cx.set_global(panels::CanvasSelection::default());
        cx.set_global(panels::ToolState::default());
        cx.set_global(panels::PlaybackPosition::default());
        cx.set_global(panels::ViewerFrame::default());
        cx.set_global(workspace::DetachedWindowHandles(Default::default()));
        trace::init(cx);
    });

    let mut shell = AppShell::default();
    for panel in [
        PanelKind::NodeGraph,
        PanelKind::Timeline,
        PanelKind::Properties,
    ] {
        if shell.visibility().is_visible(panel) {
            let toggle = match panel {
                PanelKind::NodeGraph => CommandId::ViewToggleNodeGraph,
                PanelKind::Timeline => CommandId::ViewToggleTimeline,
                _ => CommandId::ViewToggleProperties,
            };
            shell.handle_command(toggle);
        }
    }

    let workspace_entity = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured_workspace = workspace_entity.clone();
    let window = cx.open_window(size(px(800.0), px(600.0)), move |window, cx| {
        let workspace = cx.new(|cx| RavelWorkspace::new(shell, window, cx));
        *captured_workspace.borrow_mut() = Some(workspace.clone());
        Root::new(workspace, window, cx)
    });
    let workspace = workspace_entity
        .borrow_mut()
        .take()
        .expect("workspace entity should be created");
    cx.update(|cx| {
        cx.set_global(MainWorkspace::new(window.into(), workspace.downgrade()));
    });
    cx.run_until_parked();
    WorkspaceHarness { window, workspace }
}

fn dispatch(harness: &WorkspaceHarness, command: CommandId, cx: &mut TestAppContext) {
    AnyWindowHandle::from(harness.window)
        .update(cx, |_root, window, cx| {
            harness.workspace.update(cx, |workspace, cx| {
                workspace.dispatch_command(command, window, cx);
            });
        })
        .unwrap();
}

fn project(harness: &WorkspaceHarness, cx: &App) -> Entity<ProjectState> {
    harness.workspace.read(cx).project().clone()
}

fn add_solid(harness: &WorkspaceHarness, cx: &mut TestAppContext) {
    let project = cx.update(|cx| project(harness, cx));
    project.update(cx, |project, cx| {
        assert!(project.add_layer_from_template("solid", cx).is_some());
    });
}

fn layer_count(harness: &WorkspaceHarness, cx: &TestAppContext) -> usize {
    cx.read(|cx| {
        project(harness, cx)
            .read(cx)
            .active_composition(cx)
            .expect("active composition")
            .layer_count()
    })
}

fn has_dialog(window: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
    window
        .update(cx, |_root, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

#[gpui::test]
fn new_and_open_prompt_only_when_dirty_and_cancel_is_safe(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);

    dispatch(&harness, CommandId::FileNew, cx);
    assert!(!has_dialog(harness.window.into(), cx));

    add_solid(&harness, cx);
    let before = cx.read(|cx| project(&harness, cx).read(cx).document().clone());

    dispatch(&harness, CommandId::FileNew, cx);
    assert!(has_dialog(harness.window.into(), cx));
    cx.simulate_keystrokes(harness.window.into(), "escape");
    assert!(!has_dialog(harness.window.into(), cx));
    assert_eq!(
        cx.read(|cx| project(&harness, cx).read(cx).document().clone()),
        before
    );

    dispatch(&harness, CommandId::FileOpen, cx);
    assert!(has_dialog(harness.window.into(), cx));
    cx.simulate_keystrokes(harness.window.into(), "escape");
    assert!(!has_dialog(harness.window.into(), cx));
    assert_eq!(layer_count(&harness, cx), 1);
}

#[gpui::test]
fn discard_replaces_the_dirty_document(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);
    add_solid(&harness, cx);

    dispatch(&harness, CommandId::FileNew, cx);
    assert!(has_dialog(harness.window.into(), cx));
    cx.run_until_parked();

    // Click the button where it actually rendered. A hard-coded coordinate
    // depends on the platform's font metrics and misses the button on
    // Windows, leaving the dialog open.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    let bounds = visual
        .debug_bounds("unsaved-discard")
        .expect("the discard button is painted while the dialog is open");
    visual.simulate_click(bounds.center(), Modifiers::default());
    drop(visual);
    cx.run_until_parked();

    assert!(!has_dialog(harness.window.into(), cx));
    assert_eq!(layer_count(&harness, cx), 0);
    assert!(!cx.read(|cx| project(&harness, cx).read(cx).is_dirty()));
}

#[gpui::test]
fn save_completes_before_new_replaces_the_document(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);
    add_solid(&harness, cx);

    let dir = std::env::temp_dir().join(format!("ravel_guard_save_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("guard.ravprj");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.bak", path.display()));

    let project = cx.update(|cx| project(&harness, cx));
    project.update(cx, |project, cx| project.save_project_to(path.clone(), cx));
    cx.run_until_parked();
    add_solid(&harness, cx);
    assert_eq!(layer_count(&harness, cx), 2);

    dispatch(&harness, CommandId::FileNew, cx);
    cx.dispatch_keystroke(
        harness.window.into(),
        Keystroke::parse("enter").expect("enter keystroke"),
    );
    assert_eq!(layer_count(&harness, cx), 2, "New must wait for the save");
    cx.run_until_parked();

    assert_eq!(layer_count(&harness, cx), 0);
    let saved = ravel_app::project::ProjectFile::load(&path).unwrap();
    assert_eq!(
        ravel_ui::document::root_composition(&saved.document)
            .unwrap()
            .layer_count(),
        2
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.bak", path.display()));
    let _ = std::fs::remove_dir(&dir);
}

#[gpui::test]
fn failed_save_does_not_replace_the_document(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);
    add_solid(&harness, cx);

    let dir = std::env::temp_dir().join(format!("ravel_guard_fail_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("guard.ravprj");
    let project = cx.update(|cx| project(&harness, cx));
    project.update(cx, |project, cx| project.save_project_to(path.clone(), cx));
    cx.run_until_parked();
    add_solid(&harness, cx);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.bak", path.display()));
    std::fs::remove_dir(&dir).unwrap();
    std::fs::write(&dir, b"blocks the project directory").unwrap();

    dispatch(&harness, CommandId::FileNew, cx);
    cx.simulate_keystrokes(harness.window.into(), "enter");

    assert_eq!(layer_count(&harness, cx), 2);
    assert!(project.read_with(cx, |project, _| project.is_dirty()));

    std::fs::remove_file(&dir).unwrap();
}

#[gpui::test]
fn dirty_window_close_is_cancelled_and_prompts(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);
    add_solid(&harness, cx);

    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    assert!(!visual.simulate_close());
    drop(visual);

    assert!(has_dialog(harness.window.into(), cx));
    cx.simulate_keystrokes(harness.window.into(), "escape");
    assert_eq!(layer_count(&harness, cx), 1);
}

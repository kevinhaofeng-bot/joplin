//! Mounted sync flow against a real localhost server.

use crate::app::save_coordinator::ManualSaveClock;
use crate::app::{AppAction, AppModel, OpenSyncSettings, SyncNow};
use crate::library_profile::LibraryProfiles;
use crate::ui::LibraryShell;
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, SaveNote};
use app_lite_protocol::client::HttpTransport;
use app_lite_server::{ServerStore, http::HttpServer};
use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;

const TOKEN: &str = "gpui-sync-test-token-0123456789abcdef";

fn text(value: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: value.into(),
            marks: Default::default(),
        }],
    }])
}

struct Fixture {
    _root: tempfile::TempDir,
    profile: std::path::PathBuf,
    repository: Arc<LibraryRepository>,
    _server_root: tempfile::TempDir,
    server: HttpServer,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("library");
    std::fs::create_dir(&profile).unwrap();
    let repository = Arc::new(LibraryRepository::open(profile.join("library.sqlite")).unwrap());
    let server_root = tempfile::tempdir().unwrap();
    let store = Arc::new(ServerStore::open(server_root.path()).unwrap());
    let server = HttpServer::bind("127.0.0.1:0", store, TOKEN.into()).unwrap();
    Fixture {
        _root: root,
        profile,
        repository,
        _server_root: server_root,
        server,
    }
}

impl Fixture {
    fn url(&self) -> String {
        format!("http://{}", self.server.local_addr())
    }
    fn configure(&self, token: &str) {
        std::fs::write(
            self.profile.join("sync.json"),
            serde_json::to_vec(&serde_json::json!({ "server_url": self.url(), "token": token }))
                .unwrap(),
        )
        .unwrap();
    }
}

fn mount<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
) -> (gpui::Entity<LibraryShell>, &'a mut VisualTestContext) {
    let repository = Arc::clone(&fixture.repository);
    let profile = fixture.profile.clone();
    cx.update(|app| {
        crate::components::init(app);
        app.set_global(LibraryProfiles {
            base: profile.clone(),
            active: profile,
        })
    });
    let model = cx.new(move |_| AppModel::open(repository).unwrap());
    let clock = Arc::new(ManualSaveClock::default());
    cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    })
}

fn status(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> String {
    view.read_with(cx, |shell, app| shell.sync_status_text(app))
}

fn sync_now(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> String {
    cx.dispatch_action(SyncNow);
    wait_for_sync(view, cx)
}

fn wait_for_sync(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> String {
    for _ in 0..200 {
        cx.run_until_parked();
        let text = status(view, cx);
        if !text.starts_with("正在同步") {
            return text;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("sync did not finish: {}", status(view, cx));
}

#[gpui::test]
async fn unconfigured_sync_explains_how_and_settings_file_is_private(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    let text = sync_now(&view, cx);
    assert!(text.contains("未配置同步"), "{text}");
    cx.dispatch_action(OpenSyncSettings);
    cx.run_until_parked();
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(fixture.profile.join("sync.json")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

#[gpui::test]
async fn sync_now_uploads_and_the_open_note_follows_a_remote_edit(cx: &mut TestAppContext) {
    let fixture = fixture();
    fixture.configure(TOKEN);
    let note = fixture
        .repository
        .create_note(CreateNote {
            title: "同步测试".into(),
            notebook_id: None,
            document: text("本机写的"),
        })
        .unwrap();
    let (view, cx) = mount(&fixture, cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        })
    });
    cx.run_until_parked();
    assert!(
        status(&view, cx).contains("待同步"),
        "{}",
        status(&view, cx)
    );
    let first = sync_now(&view, cx);
    assert!(first.starts_with("已同步"), "{first}");
    assert!(!first.contains("待同步"), "{first}");

    // Another device edits the note that is open here.
    let other_root = tempfile::tempdir().unwrap();
    let other = LibraryRepository::open(other_root.path().join("library.sqlite")).unwrap();
    let transport = HttpTransport::new(&fixture.url(), TOKEN);
    app_lite_core::sync::sync_once(&other, &transport).unwrap();
    let remote = other.load_note(&note.id).unwrap().unwrap();
    other
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: remote.revision,
            title: "同步测试".into(),
            document: text("另一台设备改的"),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    app_lite_core::sync::sync_once(&other, &transport).unwrap();

    let second = sync_now(&view, cx);
    assert!(second.contains("下载 1"), "{second}");
    let body = view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app));
    assert!(
        body.contains("另一台设备改的"),
        "the open editor shows the new version: {body}"
    );
    let stored = fixture.repository.load_note(&note.id).unwrap().unwrap();
    let session_revision = view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .map(|session| session.read(app).expected_revision())
    });
    assert_eq!(
        session_revision,
        Some(stored.revision),
        "no stale editor revision"
    );
}

#[gpui::test]
async fn a_wrong_token_is_reported_and_nothing_is_marked_synced(cx: &mut TestAppContext) {
    let fixture = fixture();
    fixture.configure("not-the-right-token");
    fixture
        .repository
        .create_note(CreateNote {
            title: "本地".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    let (view, cx) = mount(&fixture, cx);
    let text = sync_now(&view, cx);
    assert!(text.contains("服务器拒绝了凭据"), "{text}");
    assert!(text.contains("待同步"), "{text}");
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    redraw(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is mounted"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
async fn a_refused_upload_is_listed_and_retried_only_from_its_button(cx: &mut TestAppContext) {
    let fixture = fixture();
    fixture.configure(TOKEN);
    let note = fixture
        .repository
        .create_note(CreateNote {
            title: "超大笔记".into(),
            notebook_id: None,
            document: text(&"字".repeat(app_lite_protocol::MAX_PAYLOAD_BYTES / 3 + 1)),
        })
        .unwrap();
    let (view, cx) = mount(&fixture, cx);
    let first = sync_now(&view, cx);
    assert!(first.starts_with("已同步"), "{first}");
    assert!(first.contains("1 项仅保存在本机、待同步"), "{first}");
    assert!(first.contains("1 项同步问题"), "{first}");
    let refused = fixture.repository.sync_failures().unwrap();
    assert!(
        crate::ui::sync::failure_row_text(&refused[0])
            .contains("笔记“超大笔记”：内容超过服务器单项上限")
    );

    // Another sync neither resends it nor forgets it.
    let second = sync_now(&view, cx);
    assert!(second.contains("1 项同步问题"), "{second}");
    assert_eq!(fixture.repository.sync_failures().unwrap(), refused);

    click("library-sync-status", cx);
    redraw(cx);
    assert!(cx.debug_bounds("sync-failures").is_some(), "the list opens");
    let saved = fixture.repository.load_note(&note.id).unwrap().unwrap();
    fixture
        .repository
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: saved.revision,
            title: saved.title,
            document: text("改短了"),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    click("sync-failure-retry-0", cx);
    let after = wait_for_sync(&view, cx);
    assert!(after.starts_with("已同步：上传 1"), "{after}");
    assert!(
        !after.contains("同步问题") && !after.contains("待同步"),
        "{after}"
    );
    assert!(fixture.repository.sync_failures().unwrap().is_empty());
    // gpui 0.2.2 never clears debug bounds, so the panel is checked
    // through the shell state it renders from.
    view.read_with(cx, |shell, app| {
        assert!(shell.sync_failures_open, "the list stays open");
        assert!(shell.sync_failures(app).is_empty(), "nothing left to retry");
    });

    let other_root = tempfile::tempdir().unwrap();
    let other = LibraryRepository::open(other_root.path().join("library.sqlite")).unwrap();
    app_lite_core::sync::sync_once(&other, &HttpTransport::new(&fixture.url(), TOKEN)).unwrap();
    assert!(
        other
            .load_note(&note.id)
            .unwrap()
            .unwrap()
            .body_html
            .contains("改短了")
    );
}

/// Opens `note`, types `typed` without letting it save, then applies a
/// sync pass run the way the background executor runs it.
fn type_then_finish_a_sync<'a>(
    fixture: &Fixture,
    note: &app_lite_core::NoteId,
    typed: &str,
    cx: &'a mut TestAppContext,
) -> (gpui::Entity<LibraryShell>, &'a mut VisualTestContext) {
    let (view, cx) = mount(fixture, cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.clone()), window, shell_cx);
        })
    });
    redraw(cx);
    let surface = cx.debug_bounds("native-editor-surface").unwrap();
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(typed);
    let transport = HttpTransport::new(&fixture.url(), TOKEN);
    let report = app_lite_core::sync::sync_once(&fixture.repository, &transport);
    view.update(cx, |shell, shell_cx| shell.finish_sync(report, shell_cx));
    redraw(cx);
    (view, cx)
}

/// Syncs `note` to the server and a second device, and returns that device.
fn second_device(fixture: &Fixture) -> (tempfile::TempDir, LibraryRepository) {
    let transport = HttpTransport::new(&fixture.url(), TOKEN);
    app_lite_core::sync::sync_once(&fixture.repository, &transport).unwrap();
    let root = tempfile::tempdir().unwrap();
    let other = LibraryRepository::open(root.path().join("library.sqlite")).unwrap();
    app_lite_core::sync::sync_once(&other, &transport).unwrap();
    (root, other)
}

#[gpui::test]
async fn an_unsaved_edit_overtaken_by_a_sync_is_kept_as_a_conflict_copy(cx: &mut TestAppContext) {
    let fixture = fixture();
    fixture.configure(TOKEN);
    let note = fixture
        .repository
        .create_note(CreateNote {
            title: "同一篇".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    let (_other_root, other) = second_device(&fixture);
    let remote = other.load_note(&note.id).unwrap().unwrap();
    other
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: remote.revision,
            title: "同一篇".into(),
            document: text("另一台设备改的"),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    app_lite_core::sync::sync_once(&other, &HttpTransport::new(&fixture.url(), TOKEN)).unwrap();

    let (view, cx) = type_then_finish_a_sync(&fixture, &note.id, "本机未保存", cx);
    let text = status(&view, cx);
    assert!(text.contains("另存为“同一篇（冲突副本）”"), "{text}");
    let body = view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app));
    assert!(body.contains("另一台设备改的"), "{body}");
    let bodies: Vec<(String, String)> = fixture
        .repository
        .list_notes(Default::default())
        .unwrap()
        .into_iter()
        .map(|summary| {
            let note = fixture.repository.load_note(&summary.id).unwrap().unwrap();
            (note.title, note.body_html)
        })
        .collect();
    assert!(
        bodies
            .iter()
            .any(|(title, body)| title == "同一篇（冲突副本）" && body.contains("本机未保存")),
        "{bodies:?}"
    );

    assert!(text.contains("1 个冲突副本待处理，点此查看"), "{text}");
    click("library-sync-status", cx);
    click("sync-conflict-open-0", cx);
    let copy = fixture.repository.sync_conflicts().unwrap()[0]
        .copy_id
        .clone();
    let selected = view.read_with(cx, |shell, app| {
        shell
            .model
            .read(app)
            .navigation()
            .selected_note_id()
            .cloned()
    });
    assert_eq!(selected, Some(copy.clone()), "打开 selects the copy");
    click("sync-conflict-settle-0", cx);
    assert!(fixture.repository.sync_conflicts().unwrap().is_empty());
    assert!(!status(&view, cx).contains("冲突副本待处理"));
    assert!(
        fixture.repository.load_note(&copy).unwrap().is_some(),
        "settling keeps the copy"
    );
}

#[gpui::test]
async fn a_sync_that_leaves_the_open_note_alone_keeps_its_unsaved_edit(cx: &mut TestAppContext) {
    let fixture = fixture();
    fixture.configure(TOKEN);
    let note = fixture
        .repository
        .create_note(CreateNote {
            title: "正在写".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    let (_other_root, other) = second_device(&fixture);
    other
        .create_note(CreateNote {
            title: "别的笔记".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    app_lite_core::sync::sync_once(&other, &HttpTransport::new(&fixture.url(), TOKEN)).unwrap();

    let (view, cx) = type_then_finish_a_sync(&fixture, &note.id, "继续写", cx);
    let text = status(&view, cx);
    assert!(
        text.contains("下载 1") && !text.contains("冲突副本"),
        "{text}"
    );
    let body = view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app));
    assert!(body.contains("继续写"), "the editor keeps the edit: {body}");
    cx.dispatch_action(crate::app::SyncCurrent);
    redraw(cx);
    let stored = fixture.repository.load_note(&note.id).unwrap().unwrap();
    assert!(stored.body_html.contains("继续写"), "{}", stored.body_html);
}

fn attempts(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |shell, _| shell.auto_sync_attempts_for_test())
}

/// Lets `seconds` pass on the test clock and any sync it starts finish.
fn wait(seconds: u64, view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> String {
    for _ in 0..seconds {
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        wait_for_sync(view, cx);
    }
    status(view, cx)
}

#[gpui::test]
async fn sync_runs_on_its_own_after_local_changes_and_periodically_for_remote_ones(
    cx: &mut TestAppContext,
) {
    let fixture = fixture();
    fixture.configure(TOKEN);
    fixture
        .repository
        .create_note(CreateNote {
            title: "本机写的".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    let (view, cx) = mount(&fixture, cx);
    assert!(status(&view, cx).contains("待同步"));
    let line = wait(5, &view, cx);
    assert!(
        line.starts_with("已同步") && !line.contains("待同步"),
        "{line}"
    );
    assert_eq!(attempts(&view, cx), 1);

    let (_other_root, other) = second_device(&fixture);
    let remote = other
        .create_note(CreateNote {
            title: "另一台设备的".into(),
            notebook_id: None,
            document: text("y"),
        })
        .unwrap();
    app_lite_core::sync::sync_once(&other, &HttpTransport::new(&fixture.url(), TOKEN)).unwrap();
    wait(60, &view, cx);
    assert_eq!(attempts(&view, cx), 1, "nothing to send: no sync yet");
    wait(5 * 60, &view, cx);
    assert_eq!(attempts(&view, cx), 2, "the periodic pull");
    assert!(fixture.repository.load_note(&remote.id).unwrap().is_some());
}

#[gpui::test]
async fn automatic_sync_waits_for_unsaved_input(cx: &mut TestAppContext) {
    let fixture = fixture();
    fixture.configure(TOKEN);
    let note = fixture
        .repository
        .create_note(CreateNote {
            title: "正在写".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    let (view, cx) = mount(&fixture, cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        })
    });
    redraw(cx);
    let surface = cx.debug_bounds("native-editor-surface").unwrap();
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input("还没保存");
    wait(30, &view, cx);
    assert_eq!(attempts(&view, cx), 0);
}

#[gpui::test]
async fn an_unreachable_server_backs_off_and_bad_credentials_pause_until_a_manual_sync(
    cx: &mut TestAppContext,
) {
    let fixture = fixture();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    std::fs::write(
        fixture.profile.join("sync.json"),
        serde_json::to_vec(&serde_json::json!({ "server_url": closed_url, "token": TOKEN }))
            .unwrap(),
    )
    .unwrap();
    fixture
        .repository
        .create_note(CreateNote {
            title: "等网络".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    let (view, cx) = mount(&fixture, cx);
    let line = wait(5, &view, cx);
    assert_eq!(attempts(&view, cx), 1);
    assert!(
        line.starts_with("未能连接同步服务器，稍后自动重试") && line.contains("待同步"),
        "{line}"
    );
    wait(25, &view, cx);
    assert_eq!(attempts(&view, cx), 1, "backing off 30 s");
    wait(10, &view, cx);
    assert_eq!(attempts(&view, cx), 2);
    wait(50, &view, cx);
    assert_eq!(attempts(&view, cx), 2, "then 60 s");
    wait(15, &view, cx);
    assert_eq!(attempts(&view, cx), 3);

    fixture.configure("not-the-right-token");
    wait(5 * 60, &view, cx);
    let paused = attempts(&view, cx);
    let line = wait(10 * 60, &view, cx);
    assert!(line.contains("自动同步已暂停"), "{line}");
    assert_eq!(attempts(&view, cx), paused, "no attempts while paused");

    fixture.configure(TOKEN);
    let line = sync_now(&view, cx);
    assert!(
        line.starts_with("已同步") && !line.contains("待同步"),
        "{line}"
    );
    fixture
        .repository
        .create_note(CreateNote {
            title: "之后的".into(),
            notebook_id: None,
            document: text("z"),
        })
        .unwrap();
    wait(5, &view, cx);
    assert_eq!(attempts(&view, cx), paused + 1, "resumed");
}

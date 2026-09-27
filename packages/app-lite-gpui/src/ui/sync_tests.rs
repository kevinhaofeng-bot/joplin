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
    fixture_with(app_lite_server::http::HttpOptions::default())
}

fn fixture_with(options: app_lite_server::http::HttpOptions) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("library");
    std::fs::create_dir(&profile).unwrap();
    let repository = Arc::new(LibraryRepository::open(profile.join("library.sqlite")).unwrap());
    let server_root = tempfile::tempdir().unwrap();
    let store = Arc::new(ServerStore::open(server_root.path()).unwrap());
    let server = HttpServer::bind_with("127.0.0.1:0", store, TOKEN.into(), options).unwrap();
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
async fn sync_runs_on_its_own_and_remote_edits_arrive_by_notification(cx: &mut TestAppContext) {
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
    wait_for_link(&view, cx);
    wait(60, &view, cx);
    assert_eq!(
        attempts(&view, cx),
        1,
        "no sync without a cause, not even for our own upload's notification"
    );

    // Another device's edit arrives by notification, long before the
    // periodic pull (the test clock does not move meanwhile).
    let (_other_root, other) = second_device(&fixture);
    let remote = other
        .create_note(CreateNote {
            title: "另一台设备的".into(),
            notebook_id: None,
            document: text("y"),
        })
        .unwrap();
    app_lite_core::sync::sync_once(&other, &HttpTransport::new(&fixture.url(), TOKEN)).unwrap();
    let arrived = (0..300).any(|_| {
        cx.run_until_parked();
        wait_for_sync(&view, cx);
        std::thread::sleep(std::time::Duration::from_millis(10));
        fixture.repository.load_note(&remote.id).unwrap().is_some()
    });
    assert!(arrived, "{}", status(&view, cx));
    assert_eq!(attempts(&view, cx), 2);

    wait(5 * 60, &view, cx);
    assert_eq!(
        attempts(&view, cx),
        3,
        "the periodic pull stays as a fallback"
    );
}

/// Waits (real time) for the notification link's `hello`.
fn wait_for_link(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) {
    for _ in 0..300 {
        cx.run_until_parked();
        if view.read_with(cx, |shell, _| shell.event_link_state_for_test())
            == crate::ui::sync_events::LinkState::Connected
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the notification link did not connect");
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

fn opens(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |shell, _| shell.event_link_opens_for_test())
}

fn link_state(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
) -> crate::ui::sync_events::LinkState {
    view.read_with(cx, |shell, _| shell.event_link_state_for_test())
}

/// Runs the foreground until `done` holds, in real time (link threads).
fn settle(
    view: &gpui::Entity<LibraryShell>,
    cx: &mut VisualTestContext,
    done: impl Fn(&gpui::Entity<LibraryShell>, &mut VisualTestContext) -> bool,
) {
    for _ in 0..500 {
        cx.run_until_parked();
        if done(view, cx) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("condition not reached: {:?}", link_state(view, cx));
}

#[gpui::test]
async fn the_link_reconnects_at_once_after_bye_and_backs_off_after_losing_the_server(
    cx: &mut TestAppContext,
) {
    use crate::ui::sync_events::LinkState;
    let fixture = fixture_with(app_lite_server::http::HttpOptions {
        heartbeat: std::time::Duration::from_millis(200),
        max_stream: std::time::Duration::from_secs(1),
        ..Default::default()
    });
    fixture.configure(TOKEN);
    let (view, cx) = mount(&fixture, cx);
    wait(5, &view, cx);
    wait_for_link(&view, cx);
    assert_eq!(opens(&view, cx), 1);
    // The server's scheduled `bye`: a new stream at once, no clock needed.
    settle(&view, cx, |view, cx| opens(view, cx) == 2);
    wait_for_link(&view, cx);

    drop(fixture.server);
    settle(&view, cx, |view, cx| {
        link_state(view, cx) == LinkState::Waiting
    });
    let lost = opens(&view, cx);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(2900));
    cx.run_until_parked();
    assert_eq!(opens(&view, cx), lost, "first retry after 3 s");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(200));
    settle(&view, cx, |view, cx| {
        opens(view, cx) == lost + 1 && link_state(view, cx) == LinkState::Waiting
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(4900));
    cx.run_until_parked();
    assert_eq!(
        opens(&view, cx),
        lost + 1,
        "then 5 s and up to 1 s of jitter"
    );
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(1200));
    settle(&view, cx, |view, cx| opens(view, cx) == lost + 2);
}

#[gpui::test]
async fn a_server_that_never_says_hello_is_given_up_after_15_seconds(cx: &mut TestAppContext) {
    use crate::ui::sync_events::LinkState;
    // Accepts connections and holds `/v1/events` without a byte; anything
    // else gets 503 so ordinary syncs fail fast.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = silent.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for socket in silent.incoming() {
            let Ok(mut socket) = socket else { return };
            let mut head = [0u8; 1024];
            let read = std::io::Read::read(&mut socket, &mut head).unwrap_or(0);
            if String::from_utf8_lossy(&head[..read]).contains("/v1/events") {
                held.push(socket);
            } else {
                let _ = std::io::Write::write_all(
                    &mut socket,
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        }
    });
    let fixture = fixture();
    std::fs::write(
        fixture.profile.join("sync.json"),
        serde_json::to_vec(
            &serde_json::json!({ "server_url": format!("http://{address}"), "token": TOKEN }),
        )
        .unwrap(),
    )
    .unwrap();
    let (view, cx) = mount(&fixture, cx);
    wait(5, &view, cx);
    assert_eq!(opens(&view, cx), 1);
    assert_eq!(link_state(&view, cx), LinkState::Connecting);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(14));
    cx.run_until_parked();
    assert_eq!(link_state(&view, cx), LinkState::Connecting);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(link_state(&view, cx), LinkState::Waiting);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(opens(&view, cx), 2, "retried after the first backoff step");
}

#[gpui::test]
async fn sync_and_notifications_run_over_https_with_a_trusted_self_signed_certificate(
    cx: &mut TestAppContext,
) {
    use crate::ui::sync_events::LinkState;
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_pem = certified.cert.pem();
    let tls = app_lite_server::http::tls_config_from_pem(
        certificate_pem.as_bytes(),
        certified.key_pair.serialize_pem().as_bytes(),
    )
    .unwrap();
    let fixture = fixture_with(app_lite_server::http::HttpOptions {
        tls: Some(tls),
        ..Default::default()
    });
    let https = format!("https://localhost:{}", fixture.server.local_addr().port());
    let write_config = |pem: Option<&str>| {
        std::fs::write(
            fixture.profile.join("sync.json"),
            serde_json::to_vec(&serde_json::json!({
                "server_url": https,
                "token": TOKEN,
                "server_certificate_pem": pem,
            }))
            .unwrap(),
        )
        .unwrap();
    };
    fixture
        .repository
        .create_note(CreateNote {
            title: "经 HTTPS".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    write_config(None);
    let (view, cx) = mount(&fixture, cx);
    let refused = sync_now(&view, cx);
    assert!(refused.contains("TLS 证书不受信任"), "{refused}");
    assert!(refused.contains("待同步"), "{refused}");
    wait(30, &view, cx);
    assert_ne!(
        link_state(&view, cx),
        LinkState::Connected,
        "no link to an untrusted server"
    );

    write_config(Some(&certificate_pem));
    let synced = sync_now(&view, cx);
    assert!(
        synced.starts_with("已同步") && !synced.contains("待同步"),
        "{synced}"
    );
    wait(5, &view, cx);
    wait_for_link(&view, cx);
}

#[derive(Clone)]
struct FakeEnvironment {
    wall: std::rc::Rc<std::cell::Cell<std::time::SystemTime>>,
    network: std::rc::Rc<std::cell::Cell<Option<std::net::IpAddr>>>,
}

impl crate::ui::sync_events::LinkEnvironment for FakeEnvironment {
    fn wall_clock(&self) -> std::time::SystemTime {
        self.wall.get()
    }
    fn network(&self) -> Option<std::net::IpAddr> {
        self.network.get()
    }
}

#[gpui::test]
async fn waking_from_sleep_or_changing_network_reconnects_and_syncs_at_once(
    cx: &mut TestAppContext,
) {
    let fixture = fixture();
    fixture.configure(TOKEN);
    let (view, cx) = mount(&fixture, cx);
    let environment = FakeEnvironment {
        wall: std::rc::Rc::new(std::cell::Cell::new(std::time::SystemTime::now())),
        network: std::rc::Rc::new(std::cell::Cell::new(Some("192.168.5.151".parse().unwrap()))),
    };
    view.update(cx, |shell, _| {
        shell.set_link_environment_for_test(Box::new(environment.clone()))
    });
    let recoveries = |view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext| {
        view.read_with(cx, |shell, _| shell.event_link_recoveries_for_test())
    };
    wait(5, &view, cx);
    wait_for_link(&view, cx);
    let (first_opens, first_attempts) = (opens(&view, cx), attempts(&view, cx));
    // Ordinary time passing is not a wake-up.
    for _ in 0..12 {
        environment
            .wall
            .set(environment.wall.get() + std::time::Duration::from_secs(5));
        wait(5, &view, cx);
    }
    assert_eq!(recoveries(&view, cx), 0);
    assert_eq!(opens(&view, cx), first_opens);

    environment
        .wall
        .set(environment.wall.get() + std::time::Duration::from_secs(10 * 60));
    wait(5, &view, cx);
    assert_eq!(recoveries(&view, cx), 1, "woke from sleep");
    wait_for_link(&view, cx);
    assert_eq!(opens(&view, cx), first_opens + 1);
    assert_eq!(attempts(&view, cx), first_attempts + 1, "and pulled");

    environment
        .network
        .set(Some("172.20.10.2".parse().unwrap()));
    wait(5, &view, cx);
    assert_eq!(recoveries(&view, cx), 2, "moved to another network");
    wait_for_link(&view, cx);
    assert_eq!(opens(&view, cx), first_opens + 2);
    assert_eq!(attempts(&view, cx), first_attempts + 2);
}

#[gpui::test]
async fn the_health_panel_grades_the_link_and_backlog_without_note_content(
    cx: &mut TestAppContext,
) {
    use crate::ui::sync::SyncHealth;
    let fixture = fixture_with(app_lite_server::http::HttpOptions {
        heartbeat: std::time::Duration::from_millis(200),
        ..Default::default()
    });
    let note = fixture
        .repository
        .create_note(CreateNote {
            title: "机密标题".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    let (view, cx) = mount(&fixture, cx);
    let health = |view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext| {
        view.read_with(cx, |shell, app| shell.sync_health(app))
    };
    assert_eq!(health(&view, cx).0, SyncHealth::Unconfigured);

    fixture.configure(TOKEN);
    wait(5, &view, cx);
    wait_for_link(&view, cx);
    settle(&view, cx, |view, cx| {
        view.read_with(cx, |shell, app| shell.sync_health(app))
            .1
            .iter()
            .any(|fact| fact.contains("收到服务器消息（含心跳）：0 秒前"))
    });
    let (grade, facts) = health(&view, cx);
    assert_eq!(grade, SyncHealth::Healthy, "{facts:?}");
    assert!(
        facts
            .iter()
            .any(|fact| fact.starts_with("最近一次成功同步：") && !fact.contains("尚无"))
    );
    assert!(facts.contains(&"待同步：无".to_owned()));

    drop(fixture.server);
    settle(&view, cx, |view, cx| {
        view.read_with(cx, |shell, app| shell.sync_health(app)).0 == SyncHealth::Recovering
    });
    assert!(
        health(&view, cx)
            .1
            .iter()
            .any(|fact| fact.starts_with("最近一次连接问题："))
    );

    let stored = fixture.repository.load_note(&note.id).unwrap().unwrap();
    fixture
        .repository
        .save_overtaken_edit_as_conflict_copy(&note.id, stored.revision, "机密标题", &text("y"))
        .unwrap();
    let (grade, facts) = health(&view, cx);
    assert_eq!(grade, SyncHealth::Attention);
    assert!(
        facts.iter().all(|fact| !fact.contains("机密")),
        "no note content in health facts: {facts:?}"
    );
}

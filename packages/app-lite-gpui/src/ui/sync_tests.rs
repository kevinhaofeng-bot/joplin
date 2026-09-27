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

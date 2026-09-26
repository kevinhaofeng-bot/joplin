//! Mounted import flow: pick → background stage/publish → switch.

use crate::app::save_coordinator::ManualSaveClock;
use crate::app::{AppModel, CancelLibraryImport, ImportLibrary, OpenImportedLibrary};
use crate::library_profile::LibraryProfiles;
use crate::ui::LibraryShell;
use app_lite_core::{LibraryRepository, library_counts};
use gpui::{AppContext, TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ENEX: &str = "<en-export><note><title>导入一</title><content><![CDATA[<en-note><div>正文一</div></en-note>]]></content><tag>甲</tag></note><note><title>导入二</title><content><![CDATA[<en-note><div>正文二</div></en-note>]]></content></note></en-export>";

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    source: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("library");
    std::fs::create_dir(&base).unwrap();
    let source = root.path().join("旧笔记.enex");
    std::fs::write(&source, ENEX).unwrap();
    Fixture {
        _root: root,
        base,
        source,
    }
}

fn mount<'a>(
    fixture: &Fixture,
    cx: &'a mut TestAppContext,
) -> (gpui::Entity<LibraryShell>, &'a mut VisualTestContext) {
    let repository =
        Arc::new(LibraryRepository::open(fixture.base.join("library.sqlite")).unwrap());
    let base = fixture.base.clone();
    cx.update(|app| {
        app.set_global(LibraryProfiles {
            base: base.clone(),
            active: base,
        })
    });
    let model = cx.new(move |_| AppModel::open(repository).unwrap());
    let clock = Arc::new(ManualSaveClock::default());
    cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    })
}

fn pick(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext, source: &Path) {
    cx.dispatch_action(ImportLibrary);
    cx.run_until_parked();
    let source = source.to_path_buf();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_import_picker_for_test(Ok(Some(source)), shell_cx);
        })
    });
}

fn notice(view: &gpui::Entity<LibraryShell>, cx: &mut VisualTestContext) -> String {
    view.read_with(cx, |shell, _| shell.library_import_notice_for_test())
        .unwrap_or_default()
}

#[gpui::test]
async fn mounted_import_publishes_a_new_library_and_leaves_the_active_one_untouched(
    cx: &mut TestAppContext,
) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    let before = library_counts(&fixture.base).unwrap();

    pick(&view, cx, &fixture.source);
    cx.run_until_parked();

    let message = notice(&view, cx);
    assert!(message.contains("导入完成"), "{message}");
    assert!(message.contains("2 篇笔记"), "{message}");
    let ready = view
        .read_with(cx, |shell, _| shell.imported_library_ready_for_test())
        .expect("imported library ready to open");
    assert!(ready.starts_with(fixture.base.parent().unwrap().join("imported-libraries")));
    assert_eq!(library_counts(&ready).unwrap().notes, 2);
    assert_eq!(library_counts(&fixture.base).unwrap(), before);
}

#[gpui::test]
async fn mounted_import_cancel_before_the_job_runs_creates_no_library(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);

    pick(&view, cx, &fixture.source);
    cx.dispatch_action(CancelLibraryImport);
    cx.run_until_parked();

    let message = notice(&view, cx);
    assert!(message.contains("已取消"), "{message}");
    assert!(
        view.read_with(cx, |shell, _| shell.imported_library_ready_for_test())
            .is_none()
    );
    let imports = fixture.base.parent().unwrap().join("imported-libraries");
    let libraries = std::fs::read_dir(&imports)
        .map(|entries| {
            entries
                .filter(|entry| entry.as_ref().unwrap().file_name() != ".staging")
                .count()
        })
        .unwrap_or(0);
    assert_eq!(libraries, 0);
}

#[gpui::test]
async fn mounted_open_imported_library_records_it_and_replaces_the_window(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, vcx) = mount(&fixture, cx);
    pick(&view, vcx, &fixture.source);
    vcx.run_until_parked();
    let ready = view
        .read_with(vcx, |shell, _| shell.imported_library_ready_for_test())
        .unwrap();

    vcx.dispatch_action(OpenImportedLibrary);
    vcx.run_until_parked();

    assert_eq!(crate::library_profile::resolve_active(&fixture.base), ready);
    // The mounted window is gone; read through the app, not the window.
    let app: &mut TestAppContext = vcx;
    let active = app.update(|app| app.global::<LibraryProfiles>().active.clone());
    assert_eq!(active, ready);
    let windows = app.update(|app| app.windows());
    assert_eq!(windows.len(), 1, "old window closed, new one open");
}

#[gpui::test]
async fn mounted_backup_then_restore_creates_a_new_library_and_keeps_the_active_one(
    cx: &mut TestAppContext,
) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    cx.dispatch_action(crate::app::CreateNote);
    cx.run_until_parked();
    let before = library_counts(&fixture.base).unwrap();
    let backup = fixture.base.parent().unwrap().join("整库备份");

    cx.dispatch_action(crate::app::BackupLibrary);
    cx.run_until_parked();
    let target = backup.clone();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_backup_picker_for_test(Ok(Some(target)), shell_cx);
        })
    });
    cx.run_until_parked();
    let message = notice(&view, cx);
    assert!(message.contains("备份完成"), "{message}");
    assert!(backup.join("manifest.json").is_file());

    cx.dispatch_action(crate::app::RestoreLibrary);
    cx.run_until_parked();
    let source = backup.clone();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_restore_picker_for_test(Ok(Some(source)), shell_cx);
        })
    });
    cx.run_until_parked();
    let message = notice(&view, cx);
    assert!(message.contains("恢复完成"), "{message}");
    let restored = view
        .read_with(cx, |shell, _| shell.imported_library_ready_for_test())
        .expect("restored library ready to open");
    assert!(restored.starts_with(fixture.base.parent().unwrap().join("imported-libraries")));
    assert_eq!(library_counts(&restored).unwrap(), before);
    assert_eq!(library_counts(&fixture.base).unwrap(), before);
}

#[gpui::test]
async fn mounted_copy_note_menu_action_creates_and_selects_a_copy(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    cx.dispatch_action(crate::app::CreateNote);
    cx.run_until_parked();
    let before = library_counts(&fixture.base).unwrap().notes;
    let source = view.read_with(cx, |shell, app| {
        shell
            .model
            .read(app)
            .navigation()
            .selected_note_id()
            .cloned()
    });

    cx.dispatch_action(crate::app::CopyNote);
    cx.run_until_parked();

    assert_eq!(library_counts(&fixture.base).unwrap().notes, before + 1);
    let selected = view.read_with(cx, |shell, app| {
        shell
            .model
            .read(app)
            .navigation()
            .selected_note_id()
            .cloned()
    });
    assert!(
        selected.is_some() && selected != source,
        "the copy is selected"
    );
}

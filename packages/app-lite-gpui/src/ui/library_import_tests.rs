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

#[gpui::test]
async fn mounted_restore_picker_failure_retry_and_stale_result_preserve_library_session(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    cx.dispatch_action(crate::app::CreateNote);
    cx.run_until_parked();
    let before = library_counts(&fixture.base).unwrap();
    let session_id = view.read_with(cx, |shell, _| shell.note_session.as_ref().unwrap().entity_id());
    cx.dispatch_action(crate::app::RestoreLibrary);
    let old = view.read_with(cx, |shell, _| shell.pending_library_import.as_ref().unwrap().token);
    view.update(cx, |shell, shell_cx| shell.complete_library_restore_picker(old, Err("worker unavailable".into()), shell_cx));
    assert!(view.read_with(cx, |shell, _| shell.pending_library_import.is_none()));
    cx.dispatch_action(crate::app::RestoreLibrary);
    let new = view.read_with(cx, |shell, _| shell.pending_library_import.as_ref().unwrap().token);
    assert_ne!(old, new);
    view.update(cx, |shell, shell_cx| {
        for stale in [Err("old error".into()), Ok(Some(PathBuf::from("/must-not-be-restored")))] {
            shell.complete_library_restore_picker(old, stale, shell_cx);
            assert_eq!(shell.pending_library_import.as_ref().unwrap().token, new);
        }
        shell.complete_library_restore_picker(new, Ok(None), shell_cx);
        assert!(shell.pending_library_import.is_none());
        assert_eq!(shell.note_session.as_ref().unwrap().entity_id(), session_id);
    });
    cx.run_until_parked();
    assert_eq!(library_counts(&fixture.base).unwrap(), before);
    assert!(!fixture.base.parent().unwrap().join("imported-libraries").exists());
}

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
async fn mounted_large_degradation_report_scrolls_under_the_pointer_to_its_open_button(
    cx: &mut TestAppContext,
) {
    use gpui::{MouseButton, MouseDownEvent, ScrollDelta, ScrollWheelEvent, point, px, size};
    let fixture = fixture();
    // Real importer and publication: the report grows from unsupported content,
    // not a synthetic ready flag or a mocked import result.
    let mut source = String::from("<en-export>");
    for index in 0..67 {
        source.push_str(&format!(
            "<note><title>降级报告测试{index}</title><content><![CDATA[<en-note><table><tr><td>原始内容{index}</td></tr></table></en-note>]]></content></note>"
        ));
    }
    source.push_str("</en-export>");
    std::fs::write(&fixture.source, source).unwrap();
    let (view, cx) = mount(&fixture, cx);
    pick(&view, cx, &fixture.source);
    cx.run_until_parked();
    let complete_message = notice(&view, cx);
    assert!(complete_message.contains("67 篇含暂不支持的格式"), "{complete_message}");
    assert!(complete_message.contains("第 67 篇"), "the final diagnostic is retained");
    let ready = view.read_with(cx, |shell, _| shell.imported_library_ready_for_test()).unwrap();
    assert_eq!(library_counts(&ready).unwrap().notes, 67);
    cx.simulate_resize(size(px(1160.0), px(789.0)));
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
    let rail = cx.debug_bounds("library-status-rail").unwrap();
    let report = cx.debug_bounds("library-import-status").unwrap();
    let before = cx.debug_bounds("library-import-open").unwrap();
    assert!(before.bottom() > rail.bottom(), "fixture must require reading a scrollable report");
    let pointer = point(report.right() - px(20.0), rail.top() + px(50.0));
    assert!(rail.contains(&pointer) && report.contains(&pointer));
    // Catch BlockMouse on the report: a wheel over an empty part of the rail
    // bypasses that hitbox and would miss the actual native failure.
    cx.simulate_event(ScrollWheelEvent {
        position: pointer,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-100000.0))),
        ..Default::default()
    });
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
    let open = cx.debug_bounds("library-import-open").unwrap();
    assert!(open.top() >= rail.top() && open.bottom() <= rail.bottom(),
            "wheel over the report must expose its real Open button: rail={rail:?}, button={open:?}");
    assert_eq!(notice(&view, cx), complete_message, "scrolling must not truncate diagnostics");
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: open.center(),
        click_count: 1,
        ..Default::default()
    });
    let app: &mut TestAppContext = cx;
    app.run_until_parked();
    assert_eq!(crate::library_profile::resolve_active(&fixture.base), ready);
    assert_eq!(library_counts(&fixture.base).unwrap().notes, 0);
    assert_eq!(app.update(|app| app.windows()).len(), 1);
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
async fn mounted_readable_export_then_restore_creates_a_new_library(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    cx.dispatch_action(crate::app::CreateNote);
    cx.run_until_parked();
    let before = library_counts(&fixture.base).unwrap();
    let bundle = fixture.base.parent().unwrap().join("可读导出");

    cx.dispatch_action(crate::app::ExportLibraryReadable);
    cx.run_until_parked();
    let target = bundle.clone();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_readable_export_picker_for_test(Ok(Some(target)), shell_cx);
        })
    });
    cx.run_until_parked();
    let message = notice(&view, cx);
    assert!(message.contains("可读导出完成"), "{message}");
    assert!(bundle.join("index.html").is_file());
    assert!(bundle.join("manifest.json").is_file());

    cx.dispatch_action(crate::app::RestoreLibraryReadable);
    cx.run_until_parked();
    let source = bundle.clone();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_readable_restore_picker_for_test(Ok(Some(source)), shell_cx);
        })
    });
    cx.run_until_parked();
    let message = notice(&view, cx);
    assert!(message.contains("恢复完成"), "{message}");
    let restored = view
        .read_with(cx, |shell, _| shell.imported_library_ready_for_test())
        .expect("restored library ready to open");
    assert!(restored.starts_with(fixture.base.parent().unwrap().join("imported-libraries")));
    assert_eq!(library_counts(&restored).unwrap().notes, before.notes);
    assert_eq!(library_counts(&fixture.base).unwrap(), before);
}

#[gpui::test]
async fn mounted_readable_restore_of_a_non_bundle_leaves_no_new_library(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    let bogus = fixture.base.parent().unwrap().join("不是导出");
    std::fs::create_dir_all(&bogus).unwrap();

    cx.dispatch_action(crate::app::RestoreLibraryReadable);
    cx.run_until_parked();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_readable_restore_picker_for_test(Ok(Some(bogus)), shell_cx);
        })
    });
    cx.run_until_parked();
    let message = notice(&view, cx);
    assert!(message.contains("恢复未完成"), "{message}");
    assert!(
        message.contains("不是可读导出包") && message.contains("manifest.json"),
        "says why in plain words: {message}"
    );
    assert!(
        message.contains("详情："),
        "keeps the diagnostic: {message}"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.imported_library_ready_for_test())
            .is_none()
    );
    let imports = fixture.base.parent().unwrap().join("imported-libraries");
    let leftovers = std::fs::read_dir(&imports)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        leftovers, 0,
        "a failed restore must not leave an empty library"
    );
}

#[gpui::test]
async fn mounted_readable_restore_of_a_damaged_bundle_says_what_is_wrong(cx: &mut TestAppContext) {
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    cx.dispatch_action(crate::app::CreateNote);
    cx.run_until_parked();
    let parent = fixture.base.parent().unwrap().to_path_buf();
    for (name, damage, expected) in [
        (
            "清单损坏",
            (|bundle: &std::path::Path| std::fs::write(bundle.join("manifest.json"), b"{").unwrap())
                as fn(&std::path::Path),
            "数据文件无法解析",
        ),
        (
            "历史损坏",
            |bundle: &std::path::Path| {
                let history = std::fs::read_dir(bundle.join("history"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                std::fs::write(history, b"[]").unwrap();
            },
            "未通过完整性校验",
        ),
        (
            "版本不符",
            |bundle: &std::path::Path| {
                let manifest = bundle.join("manifest.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
                value["version"] = serde_json::json!(999);
                std::fs::write(manifest, value.to_string()).unwrap();
            },
            "格式或版本不受支持",
        ),
        (
            "缺少目录",
            |bundle: &std::path::Path| std::fs::remove_dir_all(bundle.join("notes")).unwrap(),
            "导出包不完整",
        ),
    ] {
        let bundle = parent.join(name);
        cx.dispatch_action(crate::app::ExportLibraryReadable);
        cx.run_until_parked();
        let target = bundle.clone();
        cx.update(|_, app| {
            view.update(app, |shell, shell_cx| {
                shell.complete_library_readable_export_picker_for_test(Ok(Some(target)), shell_cx);
            })
        });
        cx.run_until_parked();
        assert!(notice(&view, cx).contains("可读导出完成"));
        damage(&bundle);
        cx.dispatch_action(crate::app::RestoreLibraryReadable);
        cx.run_until_parked();
        let source = bundle.clone();
        cx.update(|_, app| {
            view.update(app, |shell, shell_cx| {
                shell.complete_library_readable_restore_picker_for_test(Ok(Some(source)), shell_cx);
            })
        });
        cx.run_until_parked();
        let message = notice(&view, cx);
        assert!(message.contains(expected), "{name}: {message}");
        if name != "版本不符" {
            assert!(!message.contains("格式或版本"), "{name}: {message}");
        }
        assert!(
            message.contains("未创建新资料库") && message.contains("详情："),
            "{message}"
        );
        assert!(
            view.read_with(cx, |shell, _| shell.imported_library_ready_for_test())
                .is_none()
        );
    }
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

/// A restore's long success message and its Open button stay inside the
/// window with three, two or one columns and in a narrow window; clicking
/// the button itself (not dispatching the action) opens the restored copy.
#[gpui::test]
async fn mounted_restore_status_keeps_its_open_button_on_screen_and_clickable(
    cx: &mut TestAppContext,
) {
    use gpui::{Modifiers, point, px, size};
    let fixture = fixture();
    let (view, cx) = mount(&fixture, cx);
    cx.dispatch_action(crate::app::CreateNote);
    cx.run_until_parked();
    // A long folder name makes the message long, as real backups are.
    let backup = fixture
        .base
        .parent()
        .unwrap()
        .join("二〇二六年九月二十八日下午整理完毕的整个资料库完整备份（含全部附件与历史版本）");
    cx.dispatch_action(crate::app::BackupLibrary);
    cx.run_until_parked();
    let target = backup.clone();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_backup_picker_for_test(Ok(Some(target)), shell_cx);
        })
    });
    cx.run_until_parked();
    cx.dispatch_action(crate::app::RestoreLibrary);
    cx.run_until_parked();
    let source = backup.clone();
    cx.update(|_, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_library_restore_picker_for_test(Ok(Some(source)), shell_cx);
        })
    });
    cx.run_until_parked();
    let restored = view
        .read_with(cx, |shell, _| shell.imported_library_ready_for_test())
        .expect("restored library ready to open");

    let toggle = |cx: &mut VisualTestContext, action: crate::app::AppAction| {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(action, window, shell_cx)
            })
        });
    };
    let on_screen =
        |cx: &mut VisualTestContext, label: &str, window_size: gpui::Size<gpui::Pixels>| {
            cx.update(|window, app| window.draw(app).clear());
            cx.run_until_parked();
            let window = gpui::Bounds::new(point(px(0.0), px(0.0)), window_size);
            for selector in ["library-import-status", "library-import-open"] {
                let bounds = cx
                    .debug_bounds(selector)
                    .unwrap_or_else(|| panic!("{label}: {selector} not drawn"));
                assert!(
                    bounds.left() >= window.left()
                        && bounds.right() <= window.right()
                        && bounds.top() >= window.top()
                        && bounds.bottom() <= window.bottom(),
                    "{label}: {selector} {bounds:?} leaves the window {window:?}"
                );
            }
        };
    let wide = size(px(1160.0), px(789.0));
    cx.simulate_resize(wide);
    on_screen(cx, "three columns", wide);
    toggle(cx, crate::app::AppAction::ToggleNoteList);
    on_screen(cx, "two columns", wide);
    toggle(cx, crate::app::AppAction::ToggleSidebar);
    on_screen(cx, "one column", wide);
    toggle(cx, crate::app::AppAction::ToggleSidebar);
    toggle(cx, crate::app::AppAction::ToggleNoteList);
    let narrow = size(px(760.0), px(600.0));
    cx.simulate_resize(narrow);
    on_screen(cx, "narrow, three columns", narrow);

    let open = cx.debug_bounds("library-import-open").unwrap();
    // The button acts on mouse-down, which closes this window.
    cx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: open.center(),
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    let app: &mut TestAppContext = cx;
    app.run_until_parked();
    assert_eq!(
        crate::library_profile::resolve_active(&fixture.base),
        restored
    );
    assert_eq!(
        app.update(|app| app.windows()).len(),
        1,
        "the restored library replaced the window"
    );
}

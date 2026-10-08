use super::*;
use crate::app::AppAction;
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, LibraryRoute};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn repository() -> (tempfile::TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open temporary library"),
    );
    (profile, repository)
}

fn document(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx))
}

async fn navigate_between_two_notebooks_from_user_entry(
    use_keyboard: bool,
    cx: &mut TestAppContext,
) {
    // Catches the existing reducer being unreachable from the product, or a
    // UI callback choosing SelectNote rather than the complete history entry.
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let first_book = repository.create_notebook("前一容器", None).unwrap();
    let second_book = repository.create_notebook("后一容器", None).unwrap();
    let first = repository.create_note(CreateNote {
        title: "前一篇".into(), notebook_id: Some(first_book.id.clone()),
        document: document("前一篇完整正文"),
    }).unwrap();
    let second = repository.create_note(CreateNote {
        title: "后一篇".into(), notebook_id: Some(second_book.id.clone()),
        document: document("后一篇完整正文"),
    }).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1160.0), px(760.0)));
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        for (book, note) in [(&first_book.id, &first.id), (&second_book.id, &second.id)] {
            shell.apply_action(AppAction::NavigateTo {
                route: LibraryRoute::Notebook(book.clone()), selected_note_id: Some(note.clone()),
            }, window, shell_cx);
        }
    }));
    redraw(cx);
    if use_keyboard {
        cx.simulate_keystrokes("cmd-[");
    } else {
        let bounds = cx.debug_bounds("library-navigate-back").expect("visible history Back button");
        cx.simulate_click(bounds.center(), Modifiers::default());
    }
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &LibraryRoute::Notebook(first_book.id.clone()));
        assert_eq!(model.navigation().selected_note_id(), Some(&first.id));
        assert_eq!(shell.surface_note_id.as_ref(), Some(&first.id));
        assert_eq!(shell.note_session.as_ref().unwrap().read(app).editor().read(app).copy_all_plain_text(), "前一篇完整正文");
    });
    if use_keyboard {
        cx.simulate_keystrokes("cmd-]");
    } else {
        let bounds = cx.debug_bounds("library-navigate-forward").expect("visible history Forward button");
        cx.simulate_click(bounds.center(), Modifiers::default());
    }
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &LibraryRoute::Notebook(second_book.id.clone()));
        assert_eq!(model.navigation().selected_note_id(), Some(&second.id));
        assert_eq!(shell.surface_note_id.as_ref(), Some(&second.id));
        assert_eq!(shell.note_session.as_ref().unwrap().read(app).editor().read(app).copy_all_plain_text(), "后一篇完整正文");
    });
    assert_eq!(repository.load_note(&first.id).unwrap().unwrap().revision, first.revision);
    assert_eq!(repository.load_note(&second.id).unwrap().unwrap().revision, second.revision);
}

#[gpui::test]
async fn mounted_user_history_buttons_restore_notebook_and_note(cx: &mut TestAppContext) {
    navigate_between_two_notebooks_from_user_entry(false, cx).await;
}

#[gpui::test]
async fn mounted_user_history_shortcuts_restore_notebook_and_note(cx: &mut TestAppContext) {
    navigate_between_two_notebooks_from_user_entry(true, cx).await;
}

#[gpui::test]
async fn mounted_user_history_back_accepts_typing_without_reclicking_editor(cx: &mut TestAppContext) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let first_book = repository.create_notebook("焦点前容器", None).unwrap();
    let second_book = repository.create_notebook("焦点后容器", None).unwrap();
    let first = repository.create_note(CreateNote {
        title: "前一篇焦点".into(), notebook_id: Some(first_book.id.clone()), document: document("firstbody"),
    }).unwrap();
    let second = repository.create_note(CreateNote {
        title: "后一篇焦点".into(), notebook_id: Some(second_book.id.clone()), document: document("secondbody"),
    }).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1160.0), px(760.0)));
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        // Ordinary card selection replaces the current history entry. Use
        // two real container destinations, as the native Back scenario does.
        for (book, note) in [(&first_book.id, &first.id), (&second_book.id, &second.id)] {
            shell.apply_action(AppAction::NavigateTo {
                route: LibraryRoute::Notebook(book.clone()), selected_note_id: Some(note.clone()),
            }, window, shell_cx);
        }
    }));
    redraw(cx);
    let back = cx.debug_bounds("library-navigate-back").unwrap();
    cx.simulate_click(back.center(), Modifiers::default());
    redraw(cx);
    // The user's next keystroke is sent to the window, not injected into an
    // editor entity. A correct note ID without input focus is not usable.
    cx.simulate_input("nav294typed");
    redraw(cx);
    cx.update(|window, app| view.read_with(app, |shell, app| {
        assert_eq!(shell.surface_note_id.as_ref(), Some(&first.id));
        let editor = shell.note_session.as_ref().unwrap().read(app).editor().read(app);
        assert!(editor.copy_all_plain_text().contains("nav294typed"), "Back must accept the immediate next input without an editor click");
        assert!(editor.focus_handle().is_focused(window));
    }));
    assert_eq!(repository.load_note(&second.id).unwrap().unwrap(), second, "navigation/input must not write into the departed note");
}

#[gpui::test]
async fn mounted_user_history_forward_search_mounts_matching_body_and_focus(cx: &mut TestAppContext) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let first = repository.create_note(CreateNote {
        title: "搜索前的A".into(), notebook_id: None, document: document("firstordinarybody"),
    }).unwrap();
    let second = repository.create_note(CreateNote {
        title: "搜索命中B".into(), notebook_id: None, document: document("uniquesearch294body"),
    }).unwrap();
    repository.process_search_jobs().unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1160.0), px(760.0)));
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        shell.apply_action(AppAction::SelectNote(first.id.clone()), window, shell_cx);
    }));
    redraw(cx);
    view.update(cx, |shell, shell_cx| shell.model.update(shell_cx, |model, model_cx| {
        let query_text = "uniquesearch294body";
        let mut query = SearchQuery::parse(query_text);
        query.set_page(0, SearchQuery::MAX_PAGE_SIZE).unwrap();
        let generation = model.begin_search(query_text);
        assert!(model.commit_search_results(generation, query_text.into(), repository.search(query).unwrap(), Some(second.id.clone())).unwrap());
        model_cx.notify();
    }));
    redraw(cx);
    let back = cx.debug_bounds("library-navigate-back").unwrap();
    cx.simulate_click(back.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, _| assert_eq!(shell.surface_note_id.as_ref(), Some(&first.id)));
    let (read_sender, read_receiver) = std::sync::mpsc::channel();
    let (release_sender, release_receiver) = futures::channel::oneshot::channel();
    view.update(cx, |shell, _| shell.install_search_completion_gate_for_test(SearchCompletionGate {
        read: read_sender, release: release_receiver,
    }));
    let forward = cx.debug_bounds("library-navigate-forward").unwrap();
    cx.simulate_click(forward.center(), Modifiers::default());
    cx.run_until_parked();
    read_receiver.recv_timeout(Duration::from_secs(2)).expect("actual history worker read bounded Search packet");
    view.read_with(cx, |shell, _| assert_eq!(shell.surface_note_id.as_ref(), Some(&first.id), "old session stays coherent while search is pending"));
    release_sender.send(()).unwrap();
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().search_query(), Some("uniquesearch294body"));
        assert_eq!(model.navigation().selected_note_id(), Some(&second.id));
        assert_eq!(model.projections().len(), 1);
        assert_eq!(shell.surface_note_id.as_ref(), Some(&second.id), "selected card and mounted note must agree after asynchronous Forward");
        let session = shell.note_session.as_ref().unwrap().read(app);
        assert_eq!(session.title().read(app).text(), "搜索命中B");
        assert_eq!(session.editor().read(app).copy_all_plain_text(), "uniquesearch294body");
    });
    cx.simulate_input("forward294typed");
    redraw(cx);
    cx.update(|window, app| view.read_with(app, |shell, app| {
        let editor = shell.note_session.as_ref().unwrap().read(app).editor().read(app);
        assert!(editor.copy_all_plain_text().contains("forward294typed"));
        assert!(editor.focus_handle().is_focused(window));
    }));
    assert_eq!(repository.load_note(&first.id).unwrap().unwrap(), first, "Forward must not edit the old A");
}

#[gpui::test]
async fn mounted_sidebar_route_keeps_a_valid_selected_note_and_live_session(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive Release reproduction: the sidebar's real pointer
    // callback must carry the current typed selection into NavigateTo.  A
    // literal None clears the model packet and remounts away the live editor.
    let (_profile, repository) = repository();
    let notebook = repository
        .create_notebook("验收笔记本A", None)
        .expect("create destination notebook");
    let selected = repository
        .create_note(CreateNote {
            title: "验收三·拖拽照片".into(),
            notebook_id: Some(notebook.id.clone()),
            document: document("保留同一篇已挂载正文与撤销历史"),
        })
        .expect("create selected note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(selected.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);

    let (before_session, before_surface, before_editor, before_text, before_undo, before_history) =
        view.read_with(cx, |shell, app| {
            let model = shell.model.read(app);
            let session = shell.note_session.as_ref().expect("selected NoteSession");
            let editor = session.read(app).editor().clone();
            (
                session.entity_id(),
                shell
                    .editor_surface
                    .as_ref()
                    .expect("selected EditorSurface")
                    .entity_id(),
                editor.entity_id(),
                editor.read(app).copy_all_plain_text(),
                editor.read(app).undo_depth(),
                model.navigation().history_len_for_test(),
            )
        });
    let selector: &'static str = Box::leak(
        format!("library-sidebar-route-notebook-{}", notebook.id.as_str()).into_boxed_str(),
    );
    let target = cx
        .debug_bounds(selector)
        .expect("real destination Notebook sidebar row");
    cx.simulate_click(target.center(), Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::Notebook(notebook.id.clone())
        );
        assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
        assert_eq!(model.active_session_note_id(), Some(&selected.id));
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("retained NoteSession")
                .entity_id(),
            before_session,
            "a valid sidebar destination must not remount the selected NoteSession"
        );
        assert_eq!(
            shell
                .editor_surface
                .as_ref()
                .expect("retained EditorSurface")
                .entity_id(),
            before_surface,
            "the same selected note keeps its existing surface"
        );
        let editor = shell
            .note_session
            .as_ref()
            .expect("retained session")
            .read(app)
            .editor()
            .clone();
        assert_eq!(editor.entity_id(), before_editor);
        assert_eq!(editor.read(app).copy_all_plain_text(), before_text);
        assert_eq!(editor.read(app).undo_depth(), before_undo);
        assert_eq!(
            model.navigation().history_len_for_test(),
            before_history + 1,
            "one typed sidebar route click makes exactly one history transition"
        );
    });

    for action in [AppAction::NavigateBack, AppAction::NavigateForward] {
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(action, window, shell_cx)
            });
        });
        redraw(cx);
    }
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::Notebook(notebook.id)
        );
        assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("Forward retained session")
                .entity_id(),
            before_session
        );
        assert_eq!(
            model.navigation().history_len_for_test(),
            before_history + 1,
            "Back/Forward restores snapshots instead of appending duplicate history"
        );
    });
}

#[gpui::test]
async fn mounted_sidebar_route_flushes_a_dirty_editor_then_keeps_its_live_session_and_undo(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let notebook = repository
        .create_notebook("包含当前笔记", None)
        .expect("create destination notebook");
    let selected = repository
        .create_note(CreateNote {
            title: "保留未保存编辑的笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: document("初始正文"),
        })
        .expect("create selected note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(selected.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("selected editable surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 保留这段未保存编辑");
    redraw(cx);
    let (before_session, before_editor, before_text, before_undo) =
        view.read_with(cx, |shell, app| {
            let session = shell.note_session.as_ref().expect("dirty selected session");
            let editor = session.read(app).editor().clone();
            (
                session.entity_id(),
                editor.entity_id(),
                editor.read(app).copy_all_plain_text(),
                editor.read(app).undo_depth(),
            )
        });
    assert!(before_undo > 0, "the live edit must create undo history");
    let selector: &'static str = Box::leak(
        format!("library-sidebar-route-notebook-{}", notebook.id.as_str()).into_boxed_str(),
    );
    let target = cx
        .debug_bounds(selector)
        .expect("valid Notebook sidebar row");
    cx.simulate_click(target.center(), Modifiers::default());
    redraw(cx);

    // A route membership preflight is not a SQLite transaction. The first
    // click deliberately crosses the existing flush barrier even though this
    // note belongs to the destination: an external mutation cannot turn the
    // same ID into a dirty unmount between preflight and candidate query.
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
        assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
        let session = shell.note_session.as_ref().expect("dirty session retained");
        assert_eq!(session.entity_id(), before_session);
        let editor = session.read(app).editor().clone();
        assert_eq!(editor.entity_id(), before_editor);
        assert_eq!(editor.read(app).copy_all_plain_text(), before_text);
        assert_eq!(editor.read(app).undo_depth(), before_undo);
    });
    assert!(
        repository
            .load_note(&selected.id)
            .expect("load flushed note")
            .expect("note remains")
            .body_text
            .contains("保留这段未保存编辑"),
        "the barrier must make the dirty text durable before retrying navigation"
    );

    cx.simulate_click(target.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::Notebook(notebook.id)
        );
        assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
        let session = shell.note_session.as_ref().expect("retained dirty session");
        assert_eq!(session.entity_id(), before_session);
        let editor = session.read(app).editor().clone();
        assert_eq!(editor.entity_id(), before_editor);
        assert_eq!(editor.read(app).copy_all_plain_text(), before_text);
        assert_eq!(editor.read(app).undo_depth(), before_undo);
    });
}

#[gpui::test]
async fn mounted_sidebar_tag_route_keeps_a_valid_selected_note_and_live_session(
    cx: &mut TestAppContext,
) {
    // Evernote's SET_VIEW preserves selectedNoteGuid for a non-note route.
    // Exercise the actual typed Tag row as well as the Notebook row above:
    // the sidebar must not turn an eligible NoteId into `None` before the
    // model's authoritative projection membership check runs.
    let (_profile, repository) = repository();
    let tag = repository
        .create_tag("验收标签A")
        .expect("create target tag");
    let selected = repository
        .create_note(CreateNote {
            title: "带标签的当前笔记".into(),
            notebook_id: None,
            document: document("标签路由必须保留编辑器"),
        })
        .expect("create selected note");
    repository
        .set_note_tags(&selected.id, &[tag.id.clone()])
        .expect("tag selected note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(selected.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);

    let (before_session, before_surface, before_editor, before_text, before_undo) =
        view.read_with(cx, |shell, app| {
            let session = shell.note_session.as_ref().expect("selected NoteSession");
            let editor = session.read(app).editor().clone();
            (
                session.entity_id(),
                shell
                    .editor_surface
                    .as_ref()
                    .expect("selected EditorSurface")
                    .entity_id(),
                editor.entity_id(),
                editor.read(app).copy_all_plain_text(),
                editor.read(app).undo_depth(),
            )
        });
    let selector: &'static str =
        Box::leak(format!("library-sidebar-route-tag-{}", tag.id.as_str()).into_boxed_str());
    let target = cx
        .debug_bounds(selector)
        .expect("real destination Tag sidebar row");
    cx.simulate_click(target.center(), Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::tags(vec![tag.id.clone()]).expect("single tag route")
        );
        assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
        let session = shell.note_session.as_ref().expect("retained NoteSession");
        assert_eq!(session.entity_id(), before_session);
        assert_eq!(
            shell
                .editor_surface
                .as_ref()
                .expect("retained EditorSurface")
                .entity_id(),
            before_surface
        );
        let editor = session.read(app).editor().clone();
        assert_eq!(editor.entity_id(), before_editor);
        assert_eq!(editor.read(app).copy_all_plain_text(), before_text);
        assert_eq!(editor.read(app).undo_depth(), before_undo);
    });
}

#[gpui::test]
async fn mounted_sidebar_route_clears_selection_when_destination_excludes_it(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    let source = repository
        .create_notebook("原笔记本", None)
        .expect("create source notebook");
    let destination = repository
        .create_notebook("目标笔记本", None)
        .expect("create destination notebook");
    let selected = repository
        .create_note(CreateNote {
            title: "不属于目标的笔记".into(),
            notebook_id: Some(source.id),
            document: document("路由排除时不可显示过期正文"),
        })
        .expect("create source note");
    let destination_note = repository
        .create_note(CreateNote {
            title: "目标笔记本的笔记".into(),
            notebook_id: Some(destination.id.clone()),
            document: document("目标投影仍有自己的笔记"),
        })
        .expect("create destination note");
    let durable_before = repository
        .load_note(&selected.id)
        .expect("load source note")
        .expect("source note exists");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(selected.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);

    let selector: &'static str = Box::leak(
        format!("library-sidebar-route-notebook-{}", destination.id.as_str()).into_boxed_str(),
    );
    let target = cx
        .debug_bounds(selector)
        .expect("real excluding Notebook sidebar row");
    cx.simulate_click(target.center(), Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::Notebook(destination.id.clone())
        );
        assert_eq!(model.navigation().selected_note_id(), None);
        assert_eq!(model.active_session_note_id(), None);
        assert!(shell.note_session.is_none());
        assert!(shell.editor_surface.is_none());
        assert_eq!(
            model
                .projections()
                .iter()
                .map(|projection| projection.id.clone())
                .collect::<Vec<_>>(),
            vec![destination_note.id.clone()]
        );
    });
    assert_eq!(
        repository
            .load_note(&selected.id)
            .expect("reload source note"),
        Some(durable_before),
        "an excluded selection must clear only the presentation packet, never mutate its note"
    );
}

#[gpui::test]
async fn mounted_sidebar_exits_a_clean_durable_trash_session_without_a_second_click(
    cx: &mut TestAppContext,
) {
    // Unconditional route flushes are a dirty-session safety boundary, not a
    // reason to strand an immutable Trash preview: its clean NoteSession has
    // no semantic work and must exit on the first typed sidebar click.
    let (_profile, repository) = repository();
    let trashed = repository
        .create_note(CreateNote {
            title: "废纸篓预览".into(),
            notebook_id: None,
            document: document("只读正文"),
        })
        .expect("create note");
    repository
        .trash_note(&trashed.id)
        .expect("trash durable note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Trash,
                    selected_note_id: Some(trashed.id.clone()),
                },
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("mounted Trash session");
        assert!(session.read(app).is_durable_read_only());
    });

    let all_notes = cx
        .debug_bounds("library-sidebar-route-all-notes")
        .expect("real All Notes sidebar row");
    cx.simulate_click(all_notes.center(), Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().route(), &LibraryRoute::AllNotes);
        assert_eq!(model.navigation().selected_note_id(), None);
        assert!(shell.note_session.is_none());
        assert!(shell.editor_surface.is_none());
    });
}

#[gpui::test]
async fn mounted_sidebar_excluding_a_dirty_selection_keeps_the_existing_flush_barrier(
    cx: &mut TestAppContext,
) {
    // The model is still the membership authority, but an excluding route is
    // a real session switch.  If the sidebar submits the current ID without
    // a conservative preflight, UI lifecycle code sees equal IDs and drops a
    // dirty editor before prepare_navigation_commit clears the selection.
    let (_profile, repository) = repository();
    let source = repository
        .create_notebook("原笔记本", None)
        .expect("create source notebook");
    let destination = repository
        .create_notebook("排除目标", None)
        .expect("create destination notebook");
    let selected = repository
        .create_note(CreateNote {
            title: "未保存的原笔记".into(),
            notebook_id: Some(source.id),
            document: document("原始正文"),
        })
        .expect("create source note");
    let (view, cx) = mount_shell(repository, cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(selected.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable source surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 未保存侧栏正文");
    redraw(cx);
    let (before_session, before_text) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("dirty source session");
        let editor = session.read(app).editor().clone();
        (session.entity_id(), editor.read(app).copy_all_plain_text())
    });
    assert!(
        before_text.contains("未保存侧栏正文"),
        "the fixture must hold live unsaved editor text before switching routes"
    );

    let selector: &'static str = Box::leak(
        format!("library-sidebar-route-notebook-{}", destination.id.as_str()).into_boxed_str(),
    );
    let target = cx
        .debug_bounds(selector)
        .expect("excluding Notebook sidebar row");
    cx.simulate_click(target.center(), Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::AllNotes,
            "the first dirty route click must stop at the existing session flush barrier"
        );
        assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
        let session = shell.note_session.as_ref().expect("dirty session retained");
        assert_eq!(session.entity_id(), before_session);
        assert_eq!(
            session.read(app).editor().read(app).copy_all_plain_text(),
            before_text,
            "a rejected dirty switch must not discard the live document"
        );
    });
}

#[gpui::test]
async fn mounted_sidebar_preflight_to_projection_query_race_keeps_dirty_editor_mounted(
    cx: &mut TestAppContext,
) {
    // Deterministic TOCTOU: the real sidebar preflight sees a valid current
    // NoteId; a second writer moves the note after that read but before the
    // final projection candidate. The shell must cross the retained save
    // boundary before it can let AppModel clear an invalid same-ID selection.
    let (_profile, repository) = repository();
    let source = repository
        .create_notebook("原笔记本", None)
        .expect("create source notebook");
    let destination = repository
        .create_notebook("排除目标", None)
        .expect("create destination notebook");
    let selected = repository
        .create_note(CreateNote {
            title: "预检时仍属于目标的未保存笔记".into(),
            notebook_id: Some(destination.id.clone()),
            document: document("原始正文"),
        })
        .expect("create source note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(selected.id.clone()), window, shell_cx);
        });
    });
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editable source surface");
    cx.simulate_click(surface.center(), Modifiers::default());
    cx.simulate_input(" 竞争窗口中的未保存正文");
    redraw(cx);
    let (before_session, before_text) = view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("dirty source session");
        let editor = session.read(app).editor().clone();
        (session.entity_id(), editor.read(app).copy_all_plain_text())
    });

    let stale_sidebar_selection = view.read_with(cx, |shell, app| {
        shell
            .model
            .read(app)
            .sidebar_selected_note_for_route(&LibraryRoute::Notebook(destination.id.clone()))
            .expect("sidebar metadata preflight")
    });
    assert_eq!(
        stale_sidebar_selection,
        Some(selected.id.clone()),
        "the source-side preflight must observe membership before the competing write"
    );
    repository
        .move_selected_note(&selected.id, &source.id)
        .expect("concurrent writer moves the selected note after preflight");

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Notebook(destination.id.clone()),
                    // This is the exact stale sidebar-preflight packet. The
                    // final projection now excludes this same ID.
                    selected_note_id: stale_sidebar_selection,
                },
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);

    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            model.navigation().route(),
            &LibraryRoute::AllNotes,
            "the lifecycle gate must flush before an excluded same-ID route can unmount"
        );
        assert_eq!(model.navigation().selected_note_id(), Some(&selected.id));
        let session = shell.note_session.as_ref().expect("dirty session retained");
        assert_eq!(session.entity_id(), before_session);
        assert_eq!(
            session.read(app).editor().read(app).copy_all_plain_text(),
            before_text
        );
    });
}

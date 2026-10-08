use super::*;
use crate::app::AppAction;
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

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx))
}

fn open_organization_panel(cx: &mut VisualTestContext) {
    let toggle = cx
        .debug_bounds("library-toggle-organization")
        .expect("organization panel toggle");
    cx.simulate_click(toggle.center(), Modifiers::default());
    redraw(cx);
}

fn focus_and_type_organization_title(cx: &mut VisualTestContext, title: &str) {
    let input = cx
        .debug_bounds("library-organization-input")
        .expect("real organization EntityInputHandler");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_input(title);
    redraw(cx);
}

fn organization_input_text(view: &Entity<LibraryShell>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |shell, app| {
        shell.organization_input.read(app).text().to_owned()
    })
}

#[gpui::test]
async fn restore_destination_325_deleted_notebook_is_named_before_the_real_restore(cx: &mut TestAppContext) {
    // The transaction already falls back correctly, but the actual button's
    // label must explain where this note goes rather than promise its old book.
    let (_profile, repository) = repository();
    let default = repository.default_notebook().unwrap();
    repository.rename_notebook(&default.id, "回退目的地325").unwrap();
    let old_book = repository.create_notebook("已删除的原本325", None).unwrap();
    let resource = repository.import_resource(b"original attachment325", "original.txt", "text/plain", "txt").unwrap();
    let note = repository.create_note(app_lite_core::CreateNote {
        title: "恢复325".into(), notebook_id: Some(old_book.id.clone()),
        document: app_lite_core::CanonicalDocument::from_blocks(vec![
            app_lite_core::document::Block::Paragraph {
                style: Default::default(), inlines: vec![app_lite_core::document::Inline::Text {
                    text: "恢复必须保留这段中文和富文本".into(),
                    marks: app_lite_core::document::Marks { bold: true, underline: true, ..Default::default() },
                }],
            },
            app_lite_core::document::Block::Attachment {
                resource_id: resource.clone(), filename: "original.txt".into(), media_type: "text/plain".into(),
            },
        ]),
    }).unwrap();
    repository.delete_notebook(&old_book.id).unwrap();
    let before = repository.load_note(&note.id).unwrap().unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(900.0)));
    cx.update(|window, app| view.update(app, |shell, cx| {
        shell.apply_action(AppAction::NavigateTo {
            route: LibraryRoute::Trash, selected_note_id: Some(note.id.clone()),
        }, window, cx);
    }));
    redraw(cx);
    open_organization_panel(cx);
    let button = cx.debug_bounds("library-organization-restore-selected").expect("real restore button");
    assert_eq!(view.read_with(cx, |shell, app| shell.restore_selected_label(app)),
        "恢复到「回退目的地325」", "the rendered label must name the actual default book, not silently fall back");
    cx.simulate_click(button.center(), Modifiers::default());
    redraw(cx);
    let after = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(after.notebook_id, default.id);
    assert!(after.deleted_time.is_none());
    assert_eq!(after.body_html, before.body_html);
    assert_eq!(after.body_text, before.body_text);
    assert_eq!(after.resource_ids, before.resource_ids);
    assert_eq!(repository.read_resource_bytes(&resource).unwrap().unwrap(), b"original attachment325");
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.model.read(app).navigation().route(), &LibraryRoute::Trash);
        assert!(shell.model.read(app).navigation().selected_note_id().is_none());
    });
}

#[gpui::test]
async fn restore_destination_325_live_notebook_keeps_the_existing_restore_action(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let old_book = repository.create_notebook("仍然存在的原本325", None).unwrap();
    let note = repository.create_note(app_lite_core::CreateNote {
        title: "普通恢复325".into(), notebook_id: Some(old_book.id.clone()),
        document: app_lite_core::CanonicalDocument::default(),
    }).unwrap();
    repository.trash_note(&note.id).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1400.0), px(900.0)));
    cx.update(|window, app| view.update(app, |shell, cx| {
        shell.apply_action(AppAction::NavigateTo {
            route: LibraryRoute::Trash, selected_note_id: Some(note.id.clone()),
        }, window, cx);
    }));
    redraw(cx);
    open_organization_panel(cx);
    assert_eq!(view.read_with(cx, |shell, app| shell.restore_selected_label(app)), "恢复当前笔记");
    let button = cx.debug_bounds("library-organization-restore-selected").unwrap();
    cx.simulate_click(button.center(), Modifiers::default());
    redraw(cx);
    let restored = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(restored.notebook_id, old_book.id);
    assert!(restored.deleted_time.is_none());
}

#[gpui::test]
async fn mounted_organization_create_buttons_clear_the_shared_input_only_after_success(
    cx: &mut TestAppContext,
) {
    // A real release regression: leaving the shared TitleInput populated made
    // the next create concatenate titles (for example, "NotebookStack").
    // The three visible create controls must use one success-only clear path,
    // rather than keeping independent button-local form state.
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    open_organization_panel(cx);

    focus_and_type_organization_title(cx, "第一个笔记本");
    let create_notebook = cx
        .debug_bounds("library-organization-create-notebook")
        .expect("create notebook button");
    cx.simulate_click(create_notebook.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        organization_input_text(&view, cx),
        "",
        "a successful notebook create must clear the shared input before the next create"
    );

    focus_and_type_organization_title(cx, "第二个组");
    let create_stack = cx
        .debug_bounds("library-organization-create-stack")
        .expect("create stack button");
    cx.simulate_click(create_stack.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(organization_input_text(&view, cx), "");

    focus_and_type_organization_title(cx, "第三个标签");
    let create_tag = cx
        .debug_bounds("library-organization-create-tag")
        .expect("create tag button");
    cx.simulate_click(create_tag.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(organization_input_text(&view, cx), "");

    view.read_with(cx, |shell, app| {
        let index = shell.model.read(app).navigation_index();
        assert!(
            index
                .notebooks
                .iter()
                .any(|notebook| notebook.title == "第一个笔记本")
        );
        assert!(index.stacks.iter().any(|stack| stack.title == "第二个组"));
        assert!(index.tags.iter().any(|tag| tag.title == "第三个标签"));
    });
}

#[gpui::test]
async fn mounted_organization_create_failure_preserves_input_then_enter_uses_the_same_success_clear(
    cx: &mut TestAppContext,
) {
    // Whitespace is rejected by the real repository validator. Its text must
    // remain available for correction; then the same input's Enter path must
    // clear only after the repository accepted the corrected title.
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    open_organization_panel(cx);

    focus_and_type_organization_title(cx, "   ");
    let create_notebook = cx
        .debug_bounds("library-organization-create-notebook")
        .expect("create notebook button");
    cx.simulate_click(create_notebook.center(), Modifiers::default());
    redraw(cx);
    assert_eq!(
        organization_input_text(&view, cx),
        "   ",
        "a rejected create must retain the exact title for correction"
    );
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .model
                .read(app)
                .navigation_index()
                .notebooks
                .iter()
                .all(|notebook| notebook.title != "   ")
        );
    });

    let input = cx
        .debug_bounds("library-organization-input")
        .expect("organization input remains mounted after the rejection");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("通过 Enter 创建");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert_eq!(
        organization_input_text(&view, cx),
        "",
        "the Enter route must share the success-only input clear"
    );
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .model
                .read(app)
                .navigation_index()
                .notebooks
                .iter()
                .any(|notebook| notebook.title == "通过 Enter 创建")
        );
    });
}

fn navigate(view: &Entity<LibraryShell>, route: LibraryRoute, cx: &mut VisualTestContext) {
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route,
                    selected_note_id: None,
                },
                window,
                shell_cx,
            );
        })
    });
    redraw(cx);
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    // Per-entity selectors are built at run time; debug_bounds wants 'static.
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is mounted"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    redraw(cx);
}

fn notebook_stack(
    view: &Entity<LibraryShell>,
    notebook: &app_lite_core::NotebookId,
    cx: &VisualTestContext,
) -> Option<app_lite_core::StackId> {
    view.read_with(cx, |shell, app| {
        shell
            .model
            .read(app)
            .navigation_index()
            .notebooks
            .iter()
            .find(|candidate| &candidate.id == notebook)
            .expect("the notebook keeps its ID")
            .stack_id
            .clone()
    })
}

// Evernote's notebook menu (renderer 9435.js 42535–42610): out of a stack it
// offers "add to new stack" and each stack; in one, "remove from stack".
#[gpui::test]
async fn mounted_current_notebook_joins_leaves_and_gets_a_new_stack(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let stack = repository.create_stack("验收组").expect("create stack");
    let notebook = repository
        .create_notebook("原本", None)
        .expect("create notebook");
    let note = repository
        .create_note(app_lite_core::CreateNote {
            title: "保留原笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: app_lite_core::CanonicalDocument::default(),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    navigate(&view, LibraryRoute::Notebook(notebook.id.clone()), cx);
    open_organization_panel(cx);
    assert!(
        cx.debug_bounds("library-organization-notebook-stack")
            .is_some(),
        "the panel names the notebook it acts on"
    );
    assert!(
        cx.debug_bounds("library-organization-remove-from-stack")
            .is_none()
    );

    click(
        cx,
        &format!("library-organization-add-to-stack-{}", stack.id.as_str()),
    );
    assert_eq!(
        notebook_stack(&view, &notebook.id, cx),
        Some(stack.id.clone())
    );
    navigate(&view, LibraryRoute::Stack(stack.id.clone()), cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert!(model.projections().iter().any(|card| card.id == note.id));
    });

    navigate(&view, LibraryRoute::Notebook(notebook.id.clone()), cx);
    click(cx, "library-organization-remove-from-stack");
    assert_eq!(notebook_stack(&view, &notebook.id, cx), None);
    view.read_with(cx, |shell, app| {
        assert!(
            shell
                .model
                .read(app)
                .navigation_index()
                .stacks
                .iter()
                .any(|candidate| candidate.id == stack.id),
            "removing the notebook keeps the stack"
        );
    });

    focus_and_type_organization_title(cx, "   ");
    click(cx, "library-organization-add-to-new-stack");
    assert_eq!(
        organization_input_text(&view, cx),
        "   ",
        "kept for correction"
    );
    assert_eq!(notebook_stack(&view, &notebook.id, cx), None);
    let input = cx
        .debug_bounds("library-organization-input")
        .expect("organization input");
    cx.simulate_click(input.center(), Modifiers::default());
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("新组");
    redraw(cx);
    click(cx, "library-organization-add-to-new-stack");
    assert_eq!(organization_input_text(&view, cx), "");
    let created = view.read_with(cx, |shell, app| {
        shell
            .model
            .read(app)
            .navigation_index()
            .stacks
            .iter()
            .find(|candidate| candidate.title == "新组")
            .expect("the new stack")
            .id
            .clone()
    });
    assert_eq!(notebook_stack(&view, &notebook.id, cx), Some(created));
    assert_eq!(
        repository
            .list_navigation_index()
            .unwrap()
            .notebooks
            .iter()
            .filter(|candidate| candidate.title == "原本")
            .count(),
        1,
        "the notebook is moved, not copied"
    );
}

// The open note has unsaved Chinese text: the stack change waits behind its
// save, changes nothing first, and the text is saved to the same note.
#[gpui::test]
async fn mounted_stack_change_waits_for_the_open_notes_save(cx: &mut TestAppContext) {
    let (_profile, repository) = repository();
    let stack = repository.create_stack("组").expect("create stack");
    let notebook = repository
        .create_notebook("本", None)
        .expect("create notebook");
    let note = repository
        .create_note(app_lite_core::CreateNote {
            title: "笔记".into(),
            notebook_id: Some(notebook.id.clone()),
            document: app_lite_core::CanonicalDocument::default(),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::NavigateTo {
                    route: LibraryRoute::Notebook(notebook.id.clone()),
                    selected_note_id: Some(note.id.clone()),
                },
                window,
                shell_cx,
            );
        })
    });
    redraw(cx);
    click(cx, "native-editor-surface");
    cx.simulate_input("未保存的中文");
    redraw(cx);
    open_organization_panel(cx);
    let session = view.read_with(cx, |shell, _| {
        shell.note_session.as_ref().expect("open note").entity_id()
    });

    let add = format!("library-organization-add-to-stack-{}", stack.id.as_str());
    click(cx, &add);
    assert_eq!(
        notebook_stack(&view, &notebook.id, cx),
        None,
        "held by the save"
    );
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).navigation().selected_note_id(),
            Some(&note.id)
        );
        assert_eq!(shell.note_session.as_ref().unwrap().entity_id(), session);
    });
    cx.run_until_parked();
    redraw(cx);
    click(cx, &add);
    assert_eq!(
        notebook_stack(&view, &notebook.id, cx),
        Some(stack.id.clone())
    );
    let saved = repository.load_note(&note.id).unwrap().unwrap();
    assert!(
        saved.body_html.contains("未保存的中文"),
        "{}",
        saved.body_html
    );
    assert_eq!(saved.notebook_id, notebook.id);
}

fn find_title_and_body(
    view: &Entity<LibraryShell>,
    cx: &VisualTestContext,
) -> (String, String, Option<app_lite_core::NoteId>) {
    view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("open note").read(app);
        (
            session.title().read(app).text().to_owned(),
            session.editor().read(app).copy_all_plain_text(),
            shell
                .model
                .read(app)
                .navigation()
                .selected_note_id()
                .cloned(),
        )
    })
}

// 118: the Find panel owns every press on it. A drag in the query selects
// query text; presses on the summary, case, previous/next and the panel's
// padding never edit or select the title or body beneath, nor switch notes;
// typing after any of them goes to the query. Also at a narrow width.
#[gpui::test]
async fn find_panel_presses_stay_in_the_panel_at_any_width(cx: &mut TestAppContext) {
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(app_lite_core::CreateNote {
            title: "查找面板标题".into(),
            notebook_id: None,
            document: app_lite_core::CanonicalDocument::parse_html("<p>查找面板正文</p>").unwrap(),
        })
        .expect("create note");
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    for width in [1160.0, 640.0] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(789.0)));
        redraw(cx);
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
            })
        });
        redraw(cx);
        cx.simulate_keystrokes("cmd-f");
        redraw(cx);
        let before = find_title_and_body(&view, cx);
        cx.simulate_input("查找面板");
        redraw(cx);

        // A drag across the query selects query text, not the title.
        let query = cx
            .debug_bounds("library-find-in-note-input")
            .expect("find query");
        cx.simulate_mouse_down(
            gpui::point(query.left() + gpui::px(2.0), query.center().y),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            query.center(),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            query.center(),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        redraw(cx);
        assert!(
            view.read_with(cx, |shell, app| !shell
                .find_input
                .read(app)
                .selection()
                .is_empty()),
            "{width}: a drag selects query text"
        );

        for target in [
            "library-find-in-note-summary",
            "library-find-in-note-case",
            "library-find-in-note-previous",
            "library-find-in-note-next",
            "library-find-in-note-panel",
        ] {
            let bounds = cx
                .debug_bounds(target)
                .unwrap_or_else(|| panic!("{target} is mounted"));
            let point = if target == "library-find-in-note-panel" {
                // The panel's own padding, beside its first control.
                gpui::point(bounds.left() + gpui::px(2.0), bounds.top() + gpui::px(2.0))
            } else {
                bounds.center()
            };
            cx.simulate_click(point, Modifiers::default());
            redraw(cx);
            assert!(
                cx.update(|window, app| {
                    view.read(app)
                        .find_input
                        .read(app)
                        .focus_handle()
                        .is_focused(window)
                }),
                "{width} {target}: the query keeps the keyboard"
            );
            assert_eq!(
                find_title_and_body(&view, cx),
                before,
                "{width} {target}: nothing beneath is edited or switched"
            );
        }
        // Typing still goes to the query (Find stays open, owned by it).
        cx.simulate_input("x");
        redraw(cx);
        assert_eq!(
            find_title_and_body(&view, cx),
            before,
            "{width}: typing after the presses edits nothing beneath"
        );
        assert!(
            // It replaces the dragged selection, wherever that ended.
            view.read_with(cx, |shell, app| shell
                .find_input
                .read(app)
                .text()
                .contains('x')),
            "{width}: the keystroke went to the query"
        );
        let close = cx
            .debug_bounds("library-find-in-note-close")
            .expect("close")
            .center();
        cx.simulate_click(close, Modifiers::default());
        redraw(cx);
        assert_eq!(
            find_title_and_body(&view, cx),
            before,
            "{width}: closing Find edits nothing"
        );
    }
    let stored = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(stored.title, "查找面板标题");
}

// 130: Cmd-F "xr" in a note whose only "xr" is in a table cell finds it, and
// Enter / Cmd-G / Shift-Cmd-G step through paragraph and cell matches; the
// note's title and body stay as they were.
#[gpui::test]
async fn mounted_find_in_note_reaches_table_cells(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let note = repository
        .create_note(app_lite_core::CreateNote {
            title: "表格查找".into(),
            notebook_id: None,
            document: app_lite_core::CanonicalDocument::parse_html(
                "<p>表格前段</p><table data-joplin-lite-table=\"true\"><tbody><tr><td>甲xr</td><td>乙</td></tr></tbody></table><p>后段xr</p>",
            )
            .unwrap(),
        })
        .expect("create note");
    let stored = repository.load_note(&note.id).unwrap().unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        })
    });
    redraw(cx);
    let before = find_title_and_body(&view, cx);
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    cx.simulate_input("xr");
    redraw(cx);
    let summary = |cx: &VisualTestContext| {
        view.read_with(cx, |shell, app| {
            let editor = shell
                .note_session
                .as_ref()
                .unwrap()
                .read(app)
                .editor()
                .read(app);
            (
                editor.find_summary(),
                editor.find_primary().and_then(|found| found.cell),
            )
        })
    };
    let (found, primary) = summary(cx);
    assert_eq!(found.total, 2, "the table cell's xr and the paragraph's");
    assert_eq!(found.primary_index, Some(0));
    assert!(primary.is_some(), "the first match is in the table cell");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert_eq!(summary(cx).0.primary_index, Some(1));
    assert_eq!(summary(cx).1, None, "then the closing paragraph");
    cx.simulate_keystrokes("cmd-g");
    redraw(cx);
    assert_eq!(summary(cx).0.primary_index, Some(0), "wraps to the cell");
    cx.simulate_keystrokes("cmd-shift-g");
    redraw(cx);
    assert_eq!(summary(cx).0.primary_index, Some(1));
    assert_eq!(find_title_and_body(&view, cx), before);
    let after = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(
        (after.title, after.body_html, after.revision),
        (stored.title, stored.body_html, stored.revision)
    );
}

// 136: Evernote's actual findnext/findprev call scrollToAccent with
// block:'center' after a multi-result navigation. Minimal intersection or
// edge reveal is not that behavior. Use two matches and ample content below
// the target so scroll-range clamping cannot excuse a non-centred result.
#[gpui::test]
async fn mounted_library_table_find_centers_primary_with_scroll_room(
    cx: &mut TestAppContext,
) {
    cx.update(|app| crate::components::init(app));
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let image = repository
        .import_resource(
            include_bytes!("../../tests/resources/ocr-english.png"),
            "table-find.png",
            "image/png",
            "png",
        )
        .expect("store actual table image");
    let image = image.as_str();
    // The large first row exercises the same nested scroll-owner boundary as
    // the image-filled table in the native failure; no invented layout rects.
    let tall_cell = "表格内撑高内容<br>".repeat(45);
    let tail = "<p>表格之后的正常正文</p>".repeat(80);
    let html = format!(
        "<p>表格前段</p><table data-joplin-lite-table=\"true\"><tbody><tr><td>{tall_cell}<img src=\":/{image}\" alt=\"\"></td><td>首行末格文字</td></tr><tr><td>左末格</td><td>末格文字</td></tr></tbody></table>{tail}"
    );
    let note = repository
        .create_note(app_lite_core::CreateNote {
            title: "三栏表格定位".into(),
            notebook_id: None,
            document: app_lite_core::CanonicalDocument::parse_html(&html).unwrap(),
        })
        .expect("create tall table note");
    let stored = repository.load_note(&note.id).unwrap().unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    // Native failure: 2320x1578 device pixels, 1160x789 logical pixels.
    const WINDOW_HEIGHT: f32 = 789.0;
    cx.simulate_resize(gpui::size(gpui::px(1160.0), gpui::px(WINDOW_HEIGHT)));
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(note.id.clone()), window, shell_cx);
        })
    });
    redraw(cx);
    let before = find_title_and_body(&view, cx);
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    cx.simulate_input("末格文字");
    for _ in 0..4 {
        redraw(cx);
    }
    let viewport = view.read_with(cx, |shell, app| {
        shell.editor_surface.as_ref().unwrap().read(app).scroll_metrics_for_test().viewport
    });
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: viewport.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(10_000.0))),
        ..Default::default()
    });
    redraw(cx);
    let next = cx
        .debug_bounds("library-find-in-note-next")
        .expect("real Find Next control");
    cx.simulate_click(next.center(), Modifiers::default());
    for _ in 0..4 {
        redraw(cx);
    }
    let (summary, highlights, scroll) = view.read_with(cx, |shell, app| {
        let surface = shell.editor_surface.as_ref().unwrap().read(app);
        let editor = surface.editor().read(app);
        (
            editor.find_summary(),
            crate::native_editor::render::table_find_highlights_for_test(editor),
            surface.scroll_metrics_for_test(),
        )
    });
    assert_eq!(summary.total, 2, "two real table-cell matches");
    assert_eq!(summary.primary_index, Some(1), "Next selected the last cell");
    let primary = highlights
        .iter()
        .find(|highlight| highlight.primary)
        .expect("the real measured cell glyph has a primary highlight")
        .bounds;
    eprintln!("136 last-cell geometry: primary={primary:?}; scroll={scroll:?}");
    assert_eq!(find_title_and_body(&view, cx), before);
    let after = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(
        (after.title, after.body_html, after.revision),
        (stored.title, stored.body_html, stored.revision),
        "Find and reveal cannot write the note"
    );
    assert!(
        primary.top() >= scroll.viewport.top()
            && primary.bottom() <= gpui::px(WINDOW_HEIGHT)
            && primary.bottom() <= scroll.viewport.bottom(),
        "the whole last-cell highlight must be inside the actual window, not just the scroll owner's oversized bounds: primary={primary:?}, window_height={WINDOW_HEIGHT}, scroll={scroll:?}"
    );
    assert!(
        scroll.viewport.bottom() <= gpui::px(WINDOW_HEIGHT),
        "the editor scroll owner must not treat space below the physical window as visible: window_height={WINDOW_HEIGHT}, scroll={scroll:?}"
    );
    let center_error = f32::from((primary.center().y - scroll.viewport.center().y).abs());
    assert!(
        center_error <= f32::from(primary.size.height),
        "Evernote centers a navigated primary when there is scroll room; merely placing it at the edge is not faithful: center_error={center_error}, primary={primary:?}, scroll={scroll:?}"
    );
}

// 136 follow-up: one Find reveal lasts exactly as long as its request. It
// re-centres after a table image publishes its size, clamps at the scroll
// range's ends, and gives way to the user's scroll, to closing Find and to a
// switched note. A single result's Next does nothing, as Evernote's findnext.
// The note is the 136 fixture (above) plus a third match at the very end.
#[gpui::test]
async fn mounted_find_reveal_settles_clamps_and_yields(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let image = repository
        .import_resource(
            include_bytes!("../../tests/resources/ocr-english.png"),
            "table-find.png",
            "image/png",
            "png",
        )
        .expect("store actual table image");
    let image = image.as_str();
    let tall_cell = "表格内撑高内容<br>".repeat(45);
    let tail = "<p>表格之后的正常正文</p>".repeat(80);
    let html = format!(
        "<p>表格前段</p><table data-joplin-lite-table=\"true\"><tbody><tr><td>{tall_cell}<img src=\":/{image}\" alt=\"\"></td><td>首行末格文字</td></tr><tr><td>左末格</td><td>末格文字</td></tr></tbody></table>{tail}<p>结尾末格文字</p>"
    );
    let create = |title: &str, html: &str| {
        repository
            .create_note(app_lite_core::CreateNote {
                title: title.into(),
                notebook_id: None,
                document: app_lite_core::CanonicalDocument::parse_html(html).unwrap(),
            })
            .expect("create note")
    };
    let note = create("定位生命周期", &html);
    let single = create(
        "单一结果",
        &format!("{}<p>唯一末格文字</p>", "<p>前文</p>".repeat(120)),
    );
    let stored = repository.load_note(&note.id).unwrap().unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(gpui::px(1160.0), gpui::px(789.0)));
    redraw(cx);
    let select = |id: &app_lite_core::NoteId, cx: &mut VisualTestContext| {
        let id = id.clone();
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(AppAction::SelectNote(id), window, shell_cx);
            })
        });
        redraw(cx);
    };
    let settle = |cx: &mut VisualTestContext| {
        for _ in 0..8 {
            redraw(cx);
        }
    };
    // Primary index, its window bounds, the scroll metrics, whether a reveal
    // is still pending, and the table's image generation.
    let state = |cx: &VisualTestContext| {
        view.read_with(cx, |shell, app| {
            let surface = shell.editor_surface.as_ref().unwrap().read(app);
            let editor = surface.editor().read(app);
            let table = editor
                .document()
                .blocks()
                .iter()
                .find(|block| {
                    matches!(
                        block.content,
                        crate::native_editor::model::BlockContent::Table(_)
                    )
                })
                .map(|block| block.id);
            (
                editor.find_summary().primary_index,
                editor
                    .find_primary()
                    .and_then(|found| editor.layout().find_match_bounds(found)),
                surface.scroll_metrics_for_test(),
                surface.find_reveal_pending_for_test(),
                table
                    .and_then(|table| editor.layout().table_layout(table))
                    .map(|layout| layout.key.image_generation),
            )
        })
    };
    let undo_depth = |cx: &VisualTestContext| {
        view.read_with(cx, |shell, app| {
            shell
                .note_session
                .as_ref()
                .unwrap()
                .read(app)
                .editor()
                .read(app)
                .undo_depth()
        })
    };
    let wheel = |delta: f32, cx: &mut VisualTestContext| {
        let viewport = state(cx).2.viewport;
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(delta))),
            ..Default::default()
        });
    };
    let press = |selector: &'static str, cx: &mut VisualTestContext| {
        let bounds = cx.debug_bounds(selector).expect("real Find control");
        cx.simulate_click(bounds.center(), Modifiers::default());
    };

    select(&note.id, cx);
    let before = find_title_and_body(&view, cx);
    let undo_before = undo_depth(cx);
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    cx.simulate_input("末格文字");
    settle(cx);
    // The first result sits near the top: centring clamps at offset 0.
    let (index, primary, scroll, pending, _) = state(cx);
    let primary = primary.expect("first result measured");
    assert_eq!(index, Some(0));
    assert_eq!(scroll.offset.y, gpui::px(0.0), "top clamp");
    assert!(primary.top() >= scroll.viewport.top() && primary.bottom() <= scroll.viewport.bottom());
    assert!(!pending, "a settled reveal ends");

    // Next reaches the last table cell below the table's image: centred on
    // the geometry after the image published its size.
    wheel(10_000.0, cx);
    redraw(cx);
    press("library-find-in-note-next", cx);
    settle(cx);
    let (index, primary, scroll, pending, generation) = state(cx);
    let primary = primary.expect("cell result measured");
    assert_eq!(index, Some(1));
    assert!(
        generation.is_some_and(|generation| generation >= 1),
        "the image size was published"
    );
    let error = f32::from((primary.center().y - scroll.viewport.center().y).abs());
    assert!(
        error <= 1.0,
        "centred after the image size: error={error}, {primary:?}, {scroll:?}"
    );
    assert!(!pending);

    // The final paragraph: centring clamps at the end of the scroll range.
    press("library-find-in-note-next", cx);
    settle(cx);
    let (index, primary, scroll, _, _) = state(cx);
    let primary = primary.expect("final paragraph measured");
    assert_eq!(index, Some(2));
    assert!(
        f32::from((scroll.offset.y + scroll.max_offset.height).abs()) <= 0.5,
        "bottom clamp: {scroll:?}"
    );
    assert!(primary.top() >= scroll.viewport.top() && primary.bottom() <= scroll.viewport.bottom());

    // Next wraps to the first; Previous wraps back to the last.
    press("library-find-in-note-next", cx);
    settle(cx);
    assert_eq!(state(cx).0, Some(0));
    assert_eq!(state(cx).2.offset.y, gpui::px(0.0));
    press("library-find-in-note-previous", cx);
    settle(cx);
    assert_eq!(state(cx).0, Some(2));

    // The user's scroll right after Previous wins over the reveal.
    press("library-find-in-note-previous", cx);
    wheel(400.0, cx);
    settle(cx);
    let (index, primary, scroll, pending, _) = state(cx);
    assert_eq!(index, Some(1));
    assert!(!pending, "the user's scroll ended the request");
    let error = primary.map_or(f32::MAX, |primary| {
        f32::from((primary.center().y - scroll.viewport.center().y).abs())
    });
    assert!(error > 100.0, "not pulled back to centre: error={error}");

    // Closing Find while Next's request is still pending ends it; the offset
    // then stays.
    // (Shell calls without a frame between them; a press would run frames
    // until the reveal had already settled.)
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.navigate_find_in_note(true, shell_cx);
            let pending = |shell: &LibraryShell, app: &gpui::App| {
                shell
                    .editor_surface
                    .as_ref()
                    .unwrap()
                    .read(app)
                    .find_reveal_pending_for_test()
            };
            assert!(
                pending(shell, shell_cx),
                "Next's reveal is pending until a pass confirms it"
            );
            shell.close_find_in_note_visibility(window, shell_cx);
            assert!(!pending(shell, shell_cx), "closing Find ended the request");
        })
    });
    let closed = state(cx);
    settle(cx);
    assert_eq!(state(cx).2.offset, closed.2.offset);

    assert_eq!(find_title_and_body(&view, cx), before);
    assert_eq!(undo_depth(cx), undo_before, "Find never adds an undo step");
    let after = repository.load_note(&note.id).unwrap().unwrap();
    assert_eq!(
        (after.title, after.body_html, after.revision),
        (stored.title, stored.body_html, stored.revision)
    );

    // A request still pending when the note switches does not follow it.
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    press("library-find-in-note-next", cx);
    select(&single.id, cx);
    settle(cx);
    assert!(!state(cx).3);

    // A single result: Next and Previous neither move it nor scroll.
    let body = cx
        .debug_bounds("native-editor-surface")
        .expect("the switched note's body");
    // The surface is taller than the window; press inside what shows.
    cx.simulate_click(
        body.origin + gpui::point(gpui::px(200.0), gpui::px(60.0)),
        Modifiers::default(),
    );
    redraw(cx);
    cx.simulate_keystrokes("cmd-f");
    redraw(cx);
    cx.simulate_input("唯一末格文字");
    settle(cx);
    assert_eq!(state(cx).0, Some(0));
    wheel(2_000.0, cx);
    settle(cx);
    let parked = state(cx).2.offset;
    press("library-find-in-note-next", cx);
    settle(cx);
    press("library-find-in-note-previous", cx);
    settle(cx);
    assert_eq!(state(cx).0, Some(0));
    assert_eq!(state(cx).2.offset, parked, "no navigation, no scroll");
    assert!(!state(cx).3);
}

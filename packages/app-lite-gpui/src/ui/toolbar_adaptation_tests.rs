use super::*;
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
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

fn select_first_note(cx: &mut VisualTestContext) {
    let card = cx
        .debug_bounds("library-note-card")
        .expect("mounted note card");
    cx.simulate_click(card.center(), Modifiers::default());
    redraw(cx);
}

fn assert_inside_toolbar(cx: &mut VisualTestContext, selector: &'static str) {
    let toolbar = cx
        .debug_bounds("library-actions")
        .expect("mounted production toolbar");
    let control = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("compact toolbar must keep {selector} reachable"));
    assert!(
        f32::from(control.left()) >= f32::from(toolbar.left())
            && f32::from(control.right()) <= f32::from(toolbar.right()),
        "{selector} must be fully reachable inside the real editor toolbar: control={control:?}, toolbar={toolbar:?}"
    );
}

#[gpui::test]
async fn mounted_compact_toolbar_uses_the_default_540px_editor_column(cx: &mut TestAppContext) {
    // The standard 220px sidebar + 360px note list leaves 540px in a 1120px
    // content window. This is deliberately distinct from the 580px copied
    // profile case below: adapting from the real content mask rather than an
    // outer-window measurement must cover both.
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "默认窄栏".into(),
            notebook_id: None,
            document: document("540px 编辑列也不能裁掉核心操作。"),
        })
        .expect("create note");
    let (_view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(size(px(1120.0), px(789.0)));
    redraw(cx);
    select_first_note(cx);

    let toolbar = cx
        .debug_bounds("library-actions")
        .expect("production toolbar");
    assert_eq!(f32::from(toolbar.size.width), 540.0);
    for selector in [
        "library-create-note",
        "library-insert-resource",
        "library-toggle-sidebar",
        "library-toggle-list",
        "library-sync-current",
        "library-toggle-organization",
        "library-toolbar-more",
    ] {
        assert_inside_toolbar(cx, selector);
    }
    let more = cx
        .debug_bounds("library-toolbar-more")
        .expect("540px compact More trigger");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-toolbar-more-view").is_some(),
        "the default compact editor column must put secondary typed actions behind More"
    );
}

#[gpui::test]
async fn mounted_compact_toolbar_keeps_primary_actions_reachable_and_routes_secondary_actions_through_more(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: the old unbounded row leaves Organization beyond
    // the 1160px default shell's editor column. Merely adding a visual label
    // without moving the actual typed actions into an interactive More menu
    // leaves either these bounds or the reducer assertion below red.
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "窄栏工具栏".into(),
            notebook_id: None,
            document: document("次要操作必须仍经同一 AppAction reducer 执行。"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(size(px(1160.0), px(789.0)));
    redraw(cx);
    select_first_note(cx);

    for selector in [
        "library-create-note",
        "library-insert-resource",
        "library-toggle-sidebar",
        "library-toggle-list",
        "library-sync-current",
        "library-toggle-organization",
        "library-toolbar-more",
    ] {
        assert_inside_toolbar(cx, selector);
    }
    // GPUI's debug-bounds registry retains selectors from a resize's prior
    // paint, so absence is not a reliable assertion here. The live More
    // trigger below and its typed reducer outcome prove the current compact
    // render owns these secondary actions instead.

    let session_before = view.read_with(cx, |shell, _| {
        shell
            .note_session
            .as_ref()
            .expect("selected note session")
            .entity_id()
    });
    let more = cx
        .debug_bounds("library-toolbar-more")
        .expect("explicit compact More trigger");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    assert!(
        cx.debug_bounds("library-toolbar-more-menu").is_some(),
        "the secondary action menu must be visible after its actual compact trigger"
    );
    let view_mode = cx
        .debug_bounds("library-toolbar-more-view")
        .expect("overflowed view action");
    cx.simulate_click(view_mode.center(), Modifiers::default());
    redraw(cx);

    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.model.read(app).list_view_mode(),
            ListViewMode::Snippets,
            "the compact More row must dispatch the existing typed view action"
        );
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("view change retains session")
                .entity_id(),
            session_before,
            "a presentation adaptation must not remount the NoteSession"
        );
    });
    assert!(
        view.read_with(cx, |shell, _| !shell.toolbar_more_open),
        "a secondary action must close its one compact menu"
    );

    // Pane controls remain direct in compact layout. Their responsive
    // reflow must preserve the selected NoteSession rather than treating a
    // presentation-width change like navigation.
    let sidebar = cx
        .debug_bounds("library-toggle-sidebar")
        .expect("direct compact sidebar control");
    cx.simulate_click(sidebar.center(), Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(!shell.model.read(app).panes().sidebar_visible);
        assert_eq!(
            shell
                .note_session
                .as_ref()
                .expect("reflow retains session")
                .entity_id(),
            session_before,
            "a compact-to-wide pane reflow must not remount the editor session"
        );
    });
}

#[gpui::test]
async fn mounted_compact_toolbar_more_has_scoped_keyboard_escape_and_outside_dismissal(
    cx: &mut TestAppContext,
) {
    // The compact menu is a presentation overlay, not an editor command. Its
    // keyboard and outside-dismissal path must leave the current editor
    // selection/history intact while returning focus to the same editor.
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "紧凑键盘操作".into(),
            notebook_id: None,
            document: document("More 打开与关闭不应修改这段正文。"),
        })
        .expect("create note");
    let (view, cx) = mount_shell(repository, cx);
    cx.simulate_resize(size(px(1160.0), px(789.0)));
    redraw(cx);
    select_first_note(cx);
    let editor = view.read_with(cx, |shell, app| {
        shell
            .note_session
            .as_ref()
            .expect("selected note session")
            .read(app)
            .editor()
            .clone()
    });
    let (selection_before, history_before) =
        editor.read_with(cx, |editor, _| (editor.selection(), editor.undo_depth()));

    cx.simulate_keystrokes("cmd-alt-m");
    redraw(cx);
    assert!(
        cx.debug_bounds("library-toolbar-more-menu").is_some(),
        "the scoped More shortcut must open the same compact menu"
    );
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, _| !shell.toolbar_more_open),
        "Escape must close the compact More menu"
    );

    let more = cx
        .debug_bounds("library-toolbar-more")
        .expect("compact More trigger remains mounted");
    cx.simulate_click(more.center(), Modifiers::default());
    redraw(cx);
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted editor surface");
    cx.simulate_click(
        gpui::point(surface.left() + px(10.0), surface.bottom() - px(10.0)),
        Modifiers::default(),
    );
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, _| !shell.toolbar_more_open),
        "the transparent compact-menu backdrop must dismiss an outside click"
    );
    editor.read_with(cx, |editor, _| {
        assert_eq!(
            editor.selection(),
            selection_before,
            "compact-menu dismissal must not let the surface replace selection"
        );
        assert_eq!(
            editor.undo_depth(),
            history_before,
            "compact-menu dismissal is not an editor history transaction"
        );
    });

    cx.simulate_keystrokes("cmd-alt-i");
    redraw(cx);
    let picker_token = view.read_with(cx, |shell, _| {
        shell
            .pending_resource_insert
            .as_ref()
            .expect("the scoped Insert shortcut captures the existing resource-picker token")
            .token
    });
    cx.update(|_window, app| {
        view.update(app, |shell, shell_cx| {
            shell.cancel_resource_picker(picker_token, shell_cx)
        });
    });

    cx.simulate_keystrokes("cmd-alt-g");
    redraw(cx);
    assert!(
        cx.debug_bounds("library-organization-input").is_some(),
        "the direct Organization entry must also have a scoped keyboard route"
    );
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(view.read_with(cx, |shell, _| !shell.organization_panel_open));
}

#[gpui::test]
async fn mounted_wide_toolbar_retains_the_full_familiar_row_without_more_duplication(
    cx: &mut TestAppContext,
) {
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "宽栏工具栏".into(),
            notebook_id: None,
            document: document("宽编辑列不需要把熟悉操作隐藏在额外菜单里。"),
        })
        .expect("create note");
    let (_view, cx) = mount_shell(repository, cx);
    // 1410 - 220 sidebar - 360 list = roughly the 830px ample editor column
    // documented by the C2 brief.
    cx.simulate_resize(size(px(1410.0), px(820.0)));
    redraw(cx);
    select_first_note(cx);

    for selector in [
        "library-insert-resource",
        "library-create-note",
        "library-trash-selected",
        "library-toggle-sidebar",
        "library-toggle-list",
        "library-cycle-view",
        "library-cycle-sort",
        "library-sync-current",
        "library-toggle-organization",
    ] {
        assert_inside_toolbar(cx, selector);
    }
    assert!(
        cx.debug_bounds("library-toolbar-more").is_none(),
        "an ample editor column retains the familiar direct row rather than duplicating it in More"
    );
}

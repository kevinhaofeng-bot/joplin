//! 120: a background refresh of the active search replaces the cards; it
//! must not close the note being edited (and with it its undo history)
//! just because the edit removed its match. The note's lifecycle, a new
//! search, the person's own navigation and late packets still decide.

use super::*;
use crate::app::AppAction;
use app_lite_core::document::Block;
use app_lite_core::{CanonicalDocument, CreateNote, LibraryEvent, ResourceId, SearchQuery};
use gpui::{Entity, TestAppContext, VisualTestContext};
use std::sync::Arc;

// Observe the actual canvas's origin and current GPU clip, not a separately
// computed viewport or a test-only method on the production input entity.
#[derive(Clone, Debug)]
pub(super) struct SearchInputPaint {
    viewport: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    origin: gpui::Point<Pixels>,
    caret_x: Pixels,
}

type SearchInputPaintProbe = Rc<RefCell<Option<SearchInputPaint>>>;
thread_local! {
    static SEARCH_INPUT_PAINT_PROBE: RefCell<Option<SearchInputPaintProbe>> = const { RefCell::new(None) };
}

struct SearchInputPaintScope(Option<SearchInputPaintProbe>);

impl Drop for SearchInputPaintScope {
    fn drop(&mut self) {
        SEARCH_INPUT_PAINT_PROBE.with(|probe| probe.replace(self.0.take()));
    }
}

fn observe_search_input_paint() -> (SearchInputPaintProbe, SearchInputPaintScope) {
    let probe = Rc::new(RefCell::new(None));
    let previous = SEARCH_INPUT_PAINT_PROBE.with(|active| active.replace(Some(Rc::clone(&probe))));
    (probe, SearchInputPaintScope(previous))
}

pub(super) fn record_search_input_paint(
    viewport: Bounds<Pixels>, clip: Bounds<Pixels>, origin: gpui::Point<Pixels>, caret_x: Pixels,
) {
    SEARCH_INPUT_PAINT_PROBE.with(|active| {
        if let Some(probe) = active.borrow().as_ref() {
            *probe.borrow_mut() = Some(SearchInputPaint { viewport, clip, origin, caret_x });
        }
    });
}

#[gpui::test]
async fn search_input_326_long_query_paints_only_inside_its_field(cx: &mut TestAppContext) {
    let (probe, _scope) = observe_search_input_paint();
    let (_profile, repository) = repository();
    let note = repository.create_note(CreateNote {
        title: "搜索范围326".into(), notebook_id: None,
        document: CanonicalDocument::parse_html("<p>搜索不能修改这段中文正文</p>").unwrap(),
    }).unwrap();
    let before = repository.load_note(&note.id).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| view.update(app, |shell, scx| shell.toggle_search_palette_visibility(window, scx)));
    let input = view.read_with(cx, |shell, _| shell.search_input.clone());
    let long_query = format!("{}📝 tag:测试 notebook:默认 终", "长中文查询".repeat(40));
    for width in [1400.0, 720.0] {
        cx.simulate_resize(size(px(width), px(900.0)));
        for query in ["", "中文📝短查询", long_query.as_str()] {
            // simulate_input("") sends no character event; clear through the
            // same real shortcut/key handler as a person, not a fake replace.
            cx.simulate_keystrokes("cmd-a backspace");
            if !query.is_empty() {
                cx.simulate_input(query);
            }
            redraw(cx);
            let paint = probe.borrow().clone().expect("real search canvas painted");
            assert!(paint.clip.left() >= paint.viewport.left() - px(1.0)
                && paint.clip.right() <= paint.viewport.right() + px(1.0)
                && paint.clip.top() >= paint.viewport.top() - px(1.0)
                && paint.clip.bottom() <= paint.viewport.bottom() + px(1.0),
                "actual text/selection/caret GPU clip must stay in the search field: {paint:?}");
            assert_eq!(input.read_with(cx, |input, _| input.text().to_owned()), query,
                "display clipping must not truncate the query or its emoji");
        }
    }
    assert_eq!(repository.load_note(&note.id).unwrap(), before, "search display cannot save a note");
}

#[gpui::test]
async fn search_input_326_long_query_keeps_active_end_and_native_indices_visible(cx: &mut TestAppContext) {
    let (probe, _scope) = observe_search_input_paint();
    let (_profile, repository) = repository();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(size(px(720.0), px(900.0)));
    cx.update(|window, app| view.update(app, |shell, scx| shell.toggle_search_palette_visibility(window, scx)));
    let input = view.read_with(cx, |shell, _| shell.search_input.clone());
    let query = format!("{}📝 终", "长中文查询".repeat(40));
    cx.simulate_input(&query);
    redraw(cx);
    let at_end = probe.borrow().clone().unwrap();
    assert!(at_end.caret_x >= at_end.viewport.left() && at_end.caret_x <= at_end.viewport.right(),
        "a long query's actual insertion caret must remain visible: {at_end:?}");
    assert!(at_end.origin.x < at_end.viewport.left(), "long text must slide rather than ellipsize");
    let end_index = cx.update(|window, app| input.update(app, |input, icx| {
        gpui::EntityInputHandler::character_index_for_point(input,
            point(at_end.caret_x, at_end.viewport.center().y), window, icx)
    }));
    assert_eq!(end_index, Some(query.encode_utf16().count()), "visible text uses the same origin as native hit testing");
    input.update(cx, |input, icx| { input.move_horizontal(false, true); icx.notify(); });
    redraw(cx);
    let reversed = probe.borrow().clone().unwrap();
    assert!(reversed.caret_x >= reversed.viewport.left() && reversed.caret_x <= reversed.viewport.right());
    let head_index = cx.update(|window, app| input.update(app, |input, icx| {
        gpui::EntityInputHandler::character_index_for_point(input,
            point(reversed.caret_x, reversed.viewport.center().y), window, icx)
    }));
    assert_eq!(head_index, Some(query.encode_utf16().count() - 1), "reversed selection follows its active side");
    input.update(cx, |input, icx| { input.move_to_edge(false, true); icx.notify(); });
    redraw(cx);
    let at_start = probe.borrow().clone().unwrap();
    assert_eq!(at_start.origin, at_start.viewport.origin, "Shift-Home exposes the active start even though the selection ends offscreen");
    let start_index = cx.update(|window, app| input.update(app, |input, icx| {
        gpui::EntityInputHandler::character_index_for_point(input,
            point(at_start.caret_x, at_start.viewport.center().y), window, icx)
    }));
    assert_eq!(start_index, Some(0));
    assert_eq!(input.read_with(cx, |input, _| input.text().to_owned()), query);
}

// Same helpers as ui/tests.rs (redraw, repository, mount_shell).
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

const NEEDLE: &str = "uniquecontentneedle";

#[gpui::test]
async fn recent_notes_305_multiline_title_and_preview_paint_only_inside_their_row(cx: &mut TestAppContext) {
    let (probe, _scope) = super::note_card::observe_card_text_paints_for_test();
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository.create_note(CreateNote {
        title: "最近305标题\n第二标题行".into(), notebook_id: None,
        document: CanonicalDocument::parse_html("<p>摘要第一段</p><p>摘要第二段</p><p>摘要第三段</p><p>摘要第四段</p>").unwrap(),
    }).unwrap();
    let before = repository.load_note(&note.id).unwrap();
    repository.write_library_shell_state(&app_lite_core::LibraryShellState {
        selected_note_id: Some(note.id.clone()), ..Default::default()
    }).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.simulate_resize(gpui::size(px(1160.0), px(789.0)));
    cx.update(|window, app| view.update(app, |shell, scx| shell.toggle_search_palette_visibility(window, scx)));
    redraw(cx);
    assert!(view.read_with(cx, |shell, _| shell.recent_notes[0].snippet.lines().count()) >= 4,
        "fixture must exercise actual repository paragraph breaks, not already flattened text");
    let row = cx.debug_bounds("library-recent-note-0").unwrap();
    let paints = probe.borrow();
    let title = paints.get("library-recent-note-title-0").expect("actual title paint");
    let snippet = paints.get("library-recent-note-snippet-0").expect("actual snippet paint");
    assert_eq!(title.painted_line_count, 1, "recent title cannot spill into preview: {title:?}");
    assert_eq!(snippet.painted_line_count, 1, "hard paragraph breaks cannot spill into the next row: {snippet:?}");
    assert!(title.painted_text.starts_with("最近305标题"));
    assert!(snippet.painted_text.starts_with("摘要第一段"));
    assert!(title.bounds.top() >= row.top() && title.bounds.bottom() <= snippet.bounds.top());
    assert!(snippet.bounds.bottom() <= row.bottom(), "paint must stay inside recent row: {snippet:?}, row={row:?}");
    assert_eq!(repository.load_note(&note.id).unwrap(), before, "preview normalization must not alter saved content");
    assert!(repository.list_recent_searches("", 128).unwrap().is_empty());
}

fn recent_note_pair(repository: &LibraryRepository) -> (app_lite_core::NoteId, app_lite_core::NoteId) {
    let mut ids = Vec::new();
    for title in ["最近305 A", "最近305 B"] {
        let note = repository.create_note(CreateNote { title: title.into(), notebook_id: None,
            document: CanonicalDocument::parse_html("<p>最近笔记正文305</p>").unwrap() }).unwrap();
        repository.write_library_shell_state(&app_lite_core::LibraryShellState {
            selected_note_id: Some(note.id.clone()), ..Default::default()
        }).unwrap();
        ids.push(note.id);
    }
    (ids.remove(0), ids.remove(0))
}

#[gpui::test]
async fn recent_notes_305_empty_query_mouse_open_mounts_exact_note_without_search_history(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let (a, b) = recent_note_pair(&repository);
    let before_a = repository.load_note(&a).unwrap();
    let before_b = repository.load_note(&b).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| view.update(app, |shell, scx| shell.toggle_search_palette_visibility(window, scx)));
    redraw(cx);
    let row = cx.debug_bounds("library-recent-note-1").expect("empty query must render the older visited NoteId, not search history");
    cx.simulate_click(row.center(), gpui::Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(!shell.search_palette_open);
        assert_eq!(shell.surface_note_id.as_ref(), Some(&a));
        assert_eq!(shell.model.read(app).active_session_note_id(), Some(&a));
        assert_eq!(shell.note_session.as_ref().unwrap().read(app).title().read(app).text(), "最近305 A");
    });
    assert_eq!(repository.load_note(&a).unwrap(), before_a);
    assert_eq!(repository.load_note(&b).unwrap(), before_b);
    assert!(repository.list_recent_searches("", 128).unwrap().is_empty());
    assert_eq!(repository.list_recent_notes(1).unwrap()[0].id, a);
}

#[gpui::test]
async fn recent_notes_305_keyboard_selects_recent_note_then_typing_reaches_that_note(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let (a, b) = recent_note_pair(&repository);
    let before_b = repository.load_note(&b).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    cx.simulate_keystrokes("cmd-k"); redraw(cx);
    cx.simulate_keystrokes("down enter"); redraw(cx);
    assert_eq!(view.read_with(cx, |shell, _| shell.surface_note_id.clone()), Some(a.clone()));
    cx.simulate_input("目标输入305"); redraw(cx);
    cx.update(|window, app| view.update(app, |shell, scx| shell.apply_action(AppAction::ManualSync, window, scx)));
    cx.run_until_parked(); redraw(cx);
    assert!(repository.load_note(&a).unwrap().unwrap().body_text.contains("目标输入305"));
    assert_eq!(repository.load_note(&b).unwrap(), before_b);
    assert!(repository.list_recent_searches("", 128).unwrap().is_empty());
}

#[gpui::test]
async fn recent_notes_305_stale_trashed_target_cannot_replace_live_editor(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let (a, b) = recent_note_pair(&repository);
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx); cx.simulate_keystrokes("cmd-k"); redraw(cx);
    let row = cx.debug_bounds("library-recent-note-1").expect("recent target row must exist before deletion");
    repository.trash_note(&a).unwrap();
    cx.simulate_click(row.center(), gpui::Modifiers::default()); redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(shell.search_palette_open, "failed exact target must stay visible, not close onto another note");
        assert_eq!(shell.surface_note_id.as_ref(), Some(&b));
        assert_eq!(shell.model.read(app).active_session_note_id(), Some(&b));
    });
    assert!(repository.load_note(&a).unwrap().unwrap().deleted_time.is_some());
}

#[gpui::test]
async fn restart_276_search_shell_schedules_and_mounts_existing_background_packet(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository.create_note(CreateNote {
        title: "重开276挂载".into(), notebook_id: None,
        document: CanonicalDocument::parse_html("<p>uniquecontentneedle</p>").unwrap(),
    }).unwrap();
    repository.process_search_jobs().unwrap();
    repository.write_library_shell_state(&app_lite_core::LibraryShellState {
        location: app_lite_core::LibraryShellLocation::Search(NEEDLE.into()),
        selected_note_id: Some(note.id.clone()), ..Default::default()
    }).unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(model.navigation().search_query(),Some(NEEDLE));
        assert_eq!(model.active_session_note_id(),Some(&note.id));
        assert_eq!(model.projections().len(),1);
        assert!(model.pending_search_refresh().is_none());
        assert!(!model.navigation().can_navigate_back());
        assert!(shell.note_session.is_some());
        assert_eq!(shell.note_session.as_ref().unwrap().read(app).title().read(app).text(), "重开276挂载");
        assert_eq!(shell.search_input.read(app).text(), NEEDLE, "restored filter must remain editable in the search input");
    });
}

#[gpui::test]
async fn recent_search_246_delete_removes_prefixes_without_closing_palette_or_editing_note(cx: &mut TestAppContext) {
    // A missing recent-search view, bubbling delete to the result/backdrop,
    // or deleting only the full query breaks a user-visible result below.
    cx.update(|app| crate::components::init(app));
    let (profile, repository) = repository();
    let sql = rusqlite::Connection::open(profile.path().join("library.sqlite")).unwrap();
    sql.execute_batch("INSERT INTO search_history VALUES('会',1,1),('会议',2,1),('会议246',3,1),('other246',0,1);").unwrap();
    let note = repository.create_note(CreateNote {
        title: "保留246".into(), notebook_id: None,
        document: CanonicalDocument::parse_html("<p>不应被查询覆盖246</p>").unwrap(),
    }).unwrap();
    let before = repository.load_note(&note.id).unwrap().unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        shell.toggle_search_palette_visibility(window, shell_cx);
    }));
    redraw(cx);
    let remove = cx.debug_bounds("library-recent-search-delete-0").expect("recent history delete must be reachable");
    cx.simulate_click(remove.center(), gpui::Modifiers::default());
    redraw(cx);
    assert!(view.read_with(cx, |shell, _| shell.search_palette_open));
    cx.update(|window, app| {
        view.read_with(app, |shell, app| {
            assert!(shell.search_input.read(app).focus_handle().is_focused(window));
            assert_eq!(shell.search_input.read(app).text(), "");
        });
    });
    let queries = sql.prepare("SELECT query FROM search_history ORDER BY query").unwrap()
        .query_map([], |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(queries, vec!["other246"]);
    assert_eq!(repository.load_note(&note.id).unwrap().unwrap(), before);
}

/// A note holding one attachment whose extracted text matches `NEEDLE`.
#[gpui::test]
async fn recent_search_246_reuse_previews_without_navigation_or_recording_then_clear_keeps_focus(cx: &mut TestAppContext) {
    cx.update(|app| crate::components::init(app));
    let (_profile, repository) = repository();
    let note = repository.create_note(CreateNote {
        title: "会议246".into(), notebook_id: None,
        document: CanonicalDocument::parse_html("<p>正文246</p>").unwrap(),
    }).unwrap();
    repository.process_search_jobs().unwrap();
    repository.record_search("会议246").unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    cx.update(|window, app| view.update(app, |shell, shell_cx| {
        shell.toggle_search_palette_visibility(window, shell_cx);
    }));
    redraw(cx);
    let before = view.read_with(cx, |shell, app| shell.model.read(app).navigation().snapshot());
    let row = cx.debug_bounds("library-recent-search-0").expect("history reuse row");
    cx.simulate_click(row.center(), gpui::Modifiers::default());
    redraw(cx);
    view.read_with(cx, |shell, app| {
        assert!(shell.search_palette_open);
        assert_eq!(shell.search_input.read(app).text(), "会议246");
        assert_eq!(shell.search_palette_results.len(), 1);
        assert_eq!(shell.search_palette_results[0].note.id, note.id);
        assert_eq!(shell.model.read(app).navigation().snapshot(), before);
    });
    assert_eq!(repository.list_recent_searches("", 128).unwrap()[0].use_count, 1);
    let clear = cx.debug_bounds("library-recent-search-clear").expect("history clear action");
    cx.simulate_click(clear.center(), gpui::Modifiers::default());
    redraw(cx);
    cx.update(|window, app| view.read_with(app, |shell, app| {
        assert!(shell.search_palette_open);
        assert!(shell.search_input.read(app).focus_handle().is_focused(window));
        assert_eq!(shell.search_input.read(app).text(), "会议246");
        assert_eq!(shell.search_palette_results.len(), 1);
        assert_eq!(shell.model.read(app).navigation().snapshot(), before);
    }));
    assert!(repository.list_recent_searches("", 128).unwrap().is_empty());
}

/// A note holding one attachment whose extracted text matches `NEEDLE`.
fn attachment_note(repository: &LibraryRepository) -> (app_lite_core::NoteId, ResourceId) {
    let resource = repository
        .import_resource(
            b"synthetic PDF fixture",
            "attachment.pdf",
            "application/pdf",
            "pdf",
        )
        .expect("import attachment");
    let note = repository
        .create_note(CreateNote {
            title: "附件笔记".into(),
            notebook_id: None,
            // As the PDF note in 120: the card, then a closing paragraph.
            document: CanonicalDocument::from_blocks(vec![
                Block::Attachment {
                    resource_id: resource.clone(),
                    filename: "attachment.pdf".into(),
                    media_type: "application/pdf".into(),
                },
                Block::Paragraph {
                    style: Default::default(),
                    inlines: vec![app_lite_core::document::Inline::Text {
                        text: "尾段".into(),
                        marks: Default::default(),
                    }],
                },
            ]),
        })
        .expect("create attachment note");
    let job = repository.take_derived_text_jobs(1).unwrap().pop().unwrap();
    assert!(repository.publish_derived_text(&job, NEEDLE).unwrap());
    repository.process_search_jobs().unwrap();
    (note.id, resource)
}

fn open_search(
    view: &Entity<LibraryShell>,
    repository: &LibraryRepository,
    selected: &app_lite_core::NoteId,
    cx: &mut VisualTestContext,
) {
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, model_cx| {
            let mut query = SearchQuery::parse(NEEDLE);
            query.set_page(0, SearchQuery::MAX_PAGE_SIZE).unwrap();
            let generation = model.begin_search(NEEDLE);
            assert!(
                model
                    .commit_search_results(
                        generation,
                        NEEDLE.into(),
                        repository.search(query).unwrap(),
                        Some(selected.clone()),
                    )
                    .unwrap()
            );
            model_cx.notify();
        });
    });
    redraw(cx);
}

/// Saves the open note, reindexes, and runs the shell's own background
/// refresh of the active search to completion.
fn save_and_refresh(
    view: &Entity<LibraryShell>,
    repository: &LibraryRepository,
    note: &app_lite_core::NoteId,
    cx: &mut VisualTestContext,
) {
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        })
    });
    cx.run_until_parked();
    repository.process_search_jobs().unwrap();
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, model_cx| {
            model
                .refresh_projection_events([LibraryEvent::NoteProjectionChanged(note.clone())])
                .unwrap();
            model_cx.notify();
        });
        shell.schedule_active_search_refresh(shell_cx);
    });
    cx.run_until_parked();
    redraw(cx);
}

struct Mounted {
    session: gpui::EntityId,
    editor: gpui::EntityId,
    selected: Option<app_lite_core::NoteId>,
    cards: usize,
    attachments: usize,
}

fn mounted(view: &Entity<LibraryShell>, cx: &VisualTestContext) -> Mounted {
    view.read_with(cx, |shell, app| {
        let session = shell.note_session.as_ref().expect("an open note");
        let editor = session.read(app).editor();
        Mounted {
            session: session.entity_id(),
            editor: editor.entity_id(),
            selected: shell
                .model
                .read(app)
                .navigation()
                .selected_note_id()
                .cloned(),
            cards: shell.model.read(app).projections().len(),
            attachments: editor
                .read(app)
                .document()
                .blocks()
                .iter()
                .filter(|block| {
                    matches!(
                        block.content,
                        crate::native_editor::model::BlockContent::Attachment { .. }
                    )
                })
                .count(),
        }
    })
}

// SearchRoute: Delete the matching attachment → the refreshed result list is
// empty but the editor (and its undo history) stays; Cmd-Z brings it back
// and it matches again; Cmd-Shift-Z and Cmd-Z work the same way.
#[gpui::test]
async fn search_refresh_keeps_the_edited_note_and_its_undo_history(cx: &mut TestAppContext) {
    // The editor's own keys (Backspace, Cmd-Z, Cmd-Shift-Z) and the shell's.
    cx.update(|app| crate::components::init(app));
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let (note, resource) = attachment_note(&repository);
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);
    open_search(&view, &repository, &note, cx);
    let start = mounted(&view, cx);
    assert_eq!(start.selected.as_ref(), Some(&note));
    assert_eq!((start.cards, start.attachments), (1, 1));

    let focus_body = |cx: &mut VisualTestContext| {
        let editor = view.read_with(cx, |shell, app| {
            shell
                .note_session
                .as_ref()
                .unwrap()
                .read(app)
                .editor()
                .clone()
        });
        cx.update(|window, app| crate::native_editor::surface::focus_editor(&editor, window, app));
        // Keys follow the focus path of a drawn frame.
        redraw(cx);
    };
    let stored_resources = |cx: &mut VisualTestContext| {
        let _ = cx;
        repository.load_note(&note).unwrap().unwrap().resource_ids
    };
    let same_owner = |now: &Mounted, step: &str| {
        assert_eq!(now.session, start.session, "{step}: the same note session");
        assert_eq!(
            now.editor, start.editor,
            "{step}: the same editor (undo owner)"
        );
        assert_eq!(now.selected.as_ref(), Some(&note), "{step}: still selected");
    };

    // As in 120: click the attachment card (selecting it), then Backspace.
    let card = view.read_with(cx, |shell, app| {
        let editor = shell
            .note_session
            .as_ref()
            .unwrap()
            .read(app)
            .editor()
            .read(app);
        let block = editor
            .document()
            .blocks()
            .iter()
            .find(|block| {
                matches!(
                    block.content,
                    crate::native_editor::model::BlockContent::Attachment { .. }
                )
            })
            .expect("the attachment block")
            .id;
        editor
            .layout()
            .block_layout(block)
            .expect("the painted attachment card")
            .bounds
    });
    cx.simulate_click(card.center(), gpui::Modifiers::default());
    redraw(cx);
    cx.simulate_keystrokes("backspace");
    redraw(cx);
    save_and_refresh(&view, &repository, &note, cx);
    let deleted = mounted(&view, cx);
    same_owner(&deleted, "Delete");
    assert_eq!(
        (deleted.cards, deleted.attachments),
        (0, 0),
        "the list is truly empty"
    );
    assert!(stored_resources(cx).is_empty());
    assert_eq!(
        view.read_with(cx, |shell, app| shell
            .model
            .read(app)
            .navigation()
            .search_query()
            .map(str::to_owned)),
        Some(NEEDLE.to_owned())
    );

    // With the list empty, the open note's editor still takes the keys.
    focus_body(cx);
    cx.simulate_keystrokes("cmd-z");
    redraw(cx);
    save_and_refresh(&view, &repository, &note, cx);
    let undone = mounted(&view, cx);
    same_owner(&undone, "Cmd-Z");
    assert_eq!(
        (undone.cards, undone.attachments),
        (1, 1),
        "it matches again"
    );
    assert_eq!(stored_resources(cx), vec![resource.clone()]);

    focus_body(cx);
    cx.simulate_keystrokes("cmd-shift-z");
    redraw(cx);
    save_and_refresh(&view, &repository, &note, cx);
    let redone = mounted(&view, cx);
    same_owner(&redone, "Cmd-Shift-Z");
    assert_eq!((redone.cards, redone.attachments), (0, 0));
    assert!(stored_resources(cx).is_empty());

    focus_body(cx);
    cx.simulate_keystrokes("cmd-z");
    redraw(cx);
    save_and_refresh(&view, &repository, &note, cx);
    let again = mounted(&view, cx);
    same_owner(&again, "second Cmd-Z");
    assert_eq!((again.cards, again.attachments), (1, 1));
    assert_eq!(stored_resources(cx), vec![resource]);
}

// The rule is narrow: a trashed note is not kept open for editing, a late
// packet from before the person switched notes changes nothing, and a new
// search or Back still decides what is shown.
#[gpui::test]
async fn search_refresh_still_follows_lifecycle_navigation_and_fences(cx: &mut TestAppContext) {
    // The editor's own keys (Backspace, Cmd-Z, Cmd-Shift-Z) and the shell's.
    cx.update(|app| crate::components::init(app));
    cx.update(bind_library_keybindings);
    let (_profile, repository) = repository();
    let (note, _resource) = attachment_note(&repository);
    let other = repository
        .create_note(CreateNote {
            title: "另一篇".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>别处</p>").unwrap(),
        })
        .unwrap();
    repository.process_search_jobs().unwrap();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    // A late packet: captured, then the person opens another note; it is
    // refused by the snapshot fence and does not take the editor back.
    open_search(&view, &repository, &note, cx);
    let late = view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, _| {
            model
                .refresh_projection_events([LibraryEvent::NoteProjectionChanged(note.clone())])
                .unwrap();
            model.pending_search_refresh().expect("a pending refresh")
        })
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::SelectNote(other.id.clone()), window, shell_cx);
        })
    });
    redraw(cx);
    let committed = view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, _| {
            model.commit_search_refresh(&late.0, &late.1, late.2, Vec::new())
        })
    });
    assert!(
        !committed.unwrap(),
        "a packet from before the switch is refused"
    );
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |shell, _| shell.surface_note_id.clone()),
        Some(other.id.clone())
    );

    // A new search replaces the old one's pending refresh.
    open_search(&view, &repository, &note, cx);
    let old = view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, _| {
            model
                .refresh_projection_events([LibraryEvent::NoteProjectionChanged(note.clone())])
                .unwrap();
            model.pending_search_refresh().expect("a pending refresh")
        })
    });
    open_search(&view, &repository, &note, cx);
    let stale = view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, _| {
            model.commit_search_refresh(&old.0, &old.1, old.2, Vec::new())
        })
    });
    assert!(!stale.unwrap(), "an older search's packet is refused");
    assert_eq!(
        view.read_with(cx, |shell, app| shell.model.read(app).projections().len()),
        1
    );

    // Trashed while open in the search: the refresh closes it rather than
    // keeping a deleted note editable.
    repository.trash_note(&note).unwrap();
    repository.process_search_jobs().unwrap();
    view.update(cx, |shell, shell_cx| {
        shell.model.update(shell_cx, |model, model_cx| {
            model
                .refresh_projection_events([LibraryEvent::NoteTrashed(note.clone())])
                .unwrap();
            model_cx.notify();
        });
        shell.schedule_active_search_refresh(shell_cx);
    });
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert!(model.projections().is_empty());
        assert_eq!(model.navigation().selected_note_id(), None);
        assert_eq!(model.active_session_note_id(), None);
        assert!(shell.note_session.is_none(), "no editor for a trashed note");
    });

    // Back is the person's navigation: the mounted editor follows whatever
    // it selects (no hidden note is held open behind it).
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::NavigateBack, window, shell_cx);
        })
    });
    cx.run_until_parked();
    redraw(cx);
    view.read_with(cx, |shell, app| {
        let model = shell.model.read(app);
        assert_eq!(
            shell.surface_note_id.as_ref(),
            model.navigation().selected_note_id()
        );
        assert_eq!(
            model.active_session_note_id(),
            model.navigation().selected_note_id()
        );
    });
}

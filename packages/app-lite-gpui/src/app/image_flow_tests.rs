//! Task 5 mounted resource-flow acceptance tests.
//!
//! These tests intentionally enter through the library picker-completion
//! boundary rather than fabricating an `EditorCore` image transaction.  The
//! reported regression was a stale library presentation: a durable resource
//! could exist while the currently mounted surface and selected card still
//! showed the pre-insert document until the person switched notes.

use crate::app::save_coordinator::ManualSaveClock;
use crate::app::{AppAction, AppModel};
use crate::native_editor::images::{ClipboardPayload, ImagePayload};
use crate::ui::LibraryShell;
use app_lite_core::LibraryRepository;
use base64::Engine as _;
use gpui::{AppContext, ImageFormat, TestAppContext, VisualTestContext};
use image::{ImageBuffer, Rgba};
use rusqlite::Connection;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn draw_once(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
}

fn repository() -> (tempfile::TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open temporary library"),
    );
    (profile, repository)
}

fn picker_png(profile: &tempfile::TempDir) -> std::path::PathBuf {
    let path = profile.path().join("picker-image.png");
    let bytes = crate::native_editor::images::ClipboardPayload::fixture_with_png_and_text("")
        .images
        .into_iter()
        .next()
        .expect("embedded PNG fixture")
        .bytes;
    std::fs::write(&path, bytes).expect("write picker image fixture");
    path
}

fn picker_pdf(profile: &tempfile::TempDir) -> std::path::PathBuf {
    let path = profile.path().join("证据.pdf");
    // Attachment bytes are opaque to the editor. The durable resource store
    // owns validation/storage; this small PDF-shaped fixture is enough to
    // exercise the real picker completion route without invoking Preview.
    std::fs::write(&path, b"%PDF-1.7\nTask5 attachment fixture\n%%EOF\n")
        .expect("write picker attachment fixture");
    path
}

fn valid_jpeg_candidate() -> Vec<u8> {
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
        2,
        3,
        Rgba([0x33, 0x66, 0x99, 0xff]),
    ))
    .write_to(&mut encoded, image::ImageFormat::Jpeg)
    .expect("encode JPEG candidate");
    encoded.into_inner()
}

#[gpui::test]
async fn mounted_picker_completion_immediately_updates_surface_cache_and_selected_card(
    cx: &mut TestAppContext,
) {
    // GPUI's test platform cannot drive the operating-system chooser itself.
    // It does, however, drive the exact async picker *completion* seam used
    // by the native chooser after a person selects a file.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let before = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    let path = picker_png(&profile);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("picker must capture the active editor DocPoint");
            shell
                .complete_resource_picker_path(path.clone(), window, shell_cx)
                .expect("picker completion must commit one complete image transaction");
        });
    });
    // The completion callback returns after scheduling only. Once its
    // retained stage/commit worker has published the single atomic outcome,
    // the *first* draw of the already-mounted surface must see the resource;
    // it cannot wait for SelectNote/reopen or a later polling frame.
    cx.run_until_parked();
    draw_once(cx);
    let pending = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        pending.cache_has_resource,
        "the first presentation frame must create a cache key for the current resource"
    );
    let pending_height = pending.measured_height;
    cx.run_until_parked();
    redraw(cx);

    let after = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        after.has_image_block,
        "the active document must contain the image"
    );
    assert!(
        after.measured_height > before.measured_height,
        "the already-mounted surface must remeasure in the same presentation cycle"
    );
    assert!(
        after.cache_has_resource,
        "the current surface must have a pending or loaded image-cache key"
    );
    assert!(
        after.cache_is_settled,
        "the same cache key must settle without selecting or reopening the note"
    );
    assert!(
        (after.measured_height - pending_height).abs() < 0.01,
        "asynchronous bytes must not change the structural image extent and make the note jump"
    );
    let image_resource_id = after
        .image_resource_id
        .clone()
        .expect("image resource id after picker completion");
    assert_eq!(
        after.card_thumbnail_id,
        Some(image_resource_id.clone()),
        "the selected card projection and document/resource association must share one ID"
    );
    assert_eq!(
        repository
            .load_note(&after.note_id)
            .expect("load committed note")
            .expect("note exists")
            .resource_ids,
        vec![image_resource_id],
        "the saved note relation must be exactly the document resource order"
    );
}

#[gpui::test]
async fn mounted_picker_stages_and_commits_on_a_retained_background_task(cx: &mut TestAppContext) {
    // Deleting the retained worker and calling `stage_resource` or the
    // SQLite snapshot from the picker completion callback makes this fail:
    // while the worker is deliberately held, a real draw must still return
    // and the repository must have no visible resource relation. Releasing
    // that same worker then performs the one durable cross-layer commit.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let before = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    let path = picker_png(&profile);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("picker captures the current full Selection");
            shell
                .complete_resource_picker_path(path.clone(), window, shell_cx)
                .expect("completion only schedules durable background work");
        });
    });
    cx.run_until_parked();
    draw_once(cx);
    let while_staged = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        cx.debug_bounds("native-editor-surface").is_some(),
        "the mounted canvas must remain drawable while file hashing/SQLite is gated"
    );
    assert!(!while_staged.has_image_block);
    assert_eq!(
        repository
            .load_note(&before.note_id)
            .expect("read note while worker is gated")
            .expect("active note exists")
            .resource_ids,
        Vec::new(),
        "staging must not publish a resource, relation, projection, or snapshot before worker completion"
    );

    release.send(()).expect("release retained resource worker");
    cx.run_until_parked();
    redraw(cx);
    let after = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        after.has_image_block,
        "the same mounted session installs the committed image"
    );
    assert_eq!(
        repository
            .load_note(&after.note_id)
            .expect("read committed note")
            .expect("active note remains")
            .resource_ids
            .len(),
        1,
        "releasing one retained worker must publish one durable image relation"
    );
}

#[gpui::test]
async fn mounted_paste_tries_a_later_native_image_candidate_on_the_retained_worker(
    cx: &mut TestAppContext,
) {
    // AppKit can expose several UTI representations for one paste.  The
    // first advertised UTI is only a hint: corrupt PNG and TIFF bytes must
    // fail worker-side sniffing, then a genuinely decodable third JPEG must
    // win without falling through to placeholder text or reopening the note.
    // Holding the resource worker proves completion itself does not
    // synchronously decode either candidate in the shell callback.
    let (_profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let bytes = valid_jpeg_candidate();
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_clipboard_payload_for_test(
                    ClipboardPayload {
                        images: vec![
                            ImagePayload::new(ImageFormat::Png, vec![0x89, 0x50, 0x4e]),
                            ImagePayload::new(ImageFormat::Tiff, vec![0x49, 0x49, 0x2a]),
                            ImagePayload::new(ImageFormat::Jpeg, bytes),
                        ],
                        text: Some("截图占位文本绝不能替代图片".into()),
                        ..ClipboardPayload::default()
                    },
                    window,
                    shell_cx,
                )
                .expect("paste callback must hand bounded candidates to the worker");
        });
    });
    cx.run_until_parked();
    draw_once(cx);
    assert!(
        view.read_with(cx, |shell, _| shell.resource_notice_for_test())
            .is_none(),
        "the callback must schedule, not synchronously reject or decode, image candidates"
    );
    assert!(
        !view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "no optimistic image is visible before the retained stage worker validates it"
    );

    release.send(()).expect("release staged paste worker");
    cx.run_until_parked();
    redraw(cx);
    let after = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        after.has_image_block,
        "a later genuinely-decodable UTI candidate must be inserted when the first hint lies"
    );
    let saved = repository
        .load_note(&after.note_id)
        .expect("load pasted note")
        .expect("note exists");
    assert_eq!(
        saved.resource_ids.len(),
        1,
        "the chosen candidate must reach the existing one-resource atomic commit"
    );
    let metadata = repository
        .resource_metadata(&saved.resource_ids[0])
        .expect("read chosen resource metadata")
        .expect("the committed candidate has resource metadata");
    assert_eq!(metadata.mime, "image/jpeg");
    assert_eq!(metadata.file_extension, "jpg");
}

#[gpui::test]
async fn mounted_all_invalid_image_candidates_publish_no_resource_or_note_side_effects(
    cx: &mut TestAppContext,
) {
    // Every UTI candidate is untrusted until worker-side decoding succeeds.
    // Replacing candidate fallback with a "best effort" first commit would
    // leave an orphan metadata row or projection/sync event here.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let before = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    let durable_before = repository
        .load_note(&before.note_id)
        .expect("load clean note")
        .expect("note exists");
    let outbox_before = repository.outbox_count().expect("count outbox");
    let events = repository.subscribe();

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_clipboard_payload_for_test(
                    ClipboardPayload {
                        images: vec![
                            ImagePayload::new(ImageFormat::Png, vec![0x89, 0x50, 0x4e]),
                            ImagePayload::new(ImageFormat::Tiff, vec![0x49, 0x49, 0x2a]),
                            ImagePayload::new(ImageFormat::Jpeg, vec![0xff, 0xd8, 0xff]),
                            ImagePayload::new(ImageFormat::Gif, b"GIF89".to_vec()),
                            ImagePayload::new(ImageFormat::Webp, b"RIFFbad".to_vec()),
                            ImagePayload::new(ImageFormat::Bmp, b"BMbad".to_vec()),
                            ImagePayload::new(ImageFormat::Svg, b"<svg".to_vec()),
                        ],
                        text: Some("绝不能退回此伴随文本".into()),
                        ..ClipboardPayload::default()
                    },
                    window,
                    shell_cx,
                )
                .expect("the callback must schedule all invalid candidates");
        });
    });
    cx.run_until_parked();
    redraw(cx);

    assert!(
        !view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "no malformed candidate may install an optimistic document atom"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.resource_notice_for_test())
            .is_some_and(|notice| notice.contains("资源未插入")),
        "the failed worker must surface the real resource error"
    );
    let durable_after = repository
        .load_note(&before.note_id)
        .expect("load note after all-invalid candidates")
        .expect("note remains");
    assert_eq!(durable_after.revision, durable_before.revision);
    assert!(durable_after.resource_ids.is_empty());
    assert_eq!(repository.outbox_count().unwrap(), outbox_before);
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    let connection = Connection::open(profile.path().join("library.sqlite")).expect("open sqlite");
    let resource_rows: i64 = connection
        .query_row("SELECT count(*) FROM resources", [], |row| row.get(0))
        .expect("count resource rows");
    let relation_rows: i64 = connection
        .query_row("SELECT count(*) FROM note_resources", [], |row| row.get(0))
        .expect("count resource relations");
    assert_eq!((resource_rows, relation_rows), (0, 0));
}

#[gpui::test]
async fn mounted_drop_defers_external_path_validation_to_the_retained_stage_worker(
    cx: &mut TestAppContext,
) {
    // A missing path is deliberately used here.  The old classifier called
    // `Path::is_file()` in the event callback and immediately turned it into
    // Unsupported.  The production route must instead hand the descriptor to
    // the gated worker, where no-follow/type/size validation can fail without
    // blocking a draw or consulting a later caret.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    let missing = profile.path().join("network-volume-not-yet-mounted.pdf");

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_drop_paths_for_test(vec![missing], window, shell_cx)
                .expect("drop callback must queue descriptor validation on the worker");
        });
    });
    cx.run_until_parked();
    draw_once(cx);
    assert!(
        cx.debug_bounds("native-editor-surface").is_some(),
        "the shared surface must still paint while path validation is gated"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.resource_notice_for_test())
            .is_none(),
        "a drop callback must not synchronously stat and reject a path"
    );

    release.send(()).expect("release descriptor validator");
    cx.run_until_parked();
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, _| shell.resource_notice_for_test())
            .is_some_and(|notice| notice.contains("资源未插入")),
        "the descriptor-safe worker, rather than the UI callback, reports the missing path"
    );
}

#[cfg(unix)]
#[gpui::test]
async fn mounted_drop_skips_an_unsafe_first_path_and_commits_the_later_valid_candidate(
    cx: &mut TestAppContext,
) {
    // Preserve Finder order, but do not trust it.  `Path::is_file()` follows
    // this symlink and used to select it in the UI callback; the secure worker
    // then rejected that one source and never saw the following PNG.  The
    // worker must own both no-follow validation and ordered fallback.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let valid = picker_png(&profile);
    let unsafe_first = profile.path().join("untrusted-first.png");
    std::os::unix::fs::symlink(&valid, &unsafe_first).expect("create untrusted symlink fixture");
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_drop_paths_for_test(vec![unsafe_first, valid], window, shell_cx)
                .expect("drop callback must retain ordered external candidates");
        });
    });
    cx.run_until_parked();
    draw_once(cx);
    release.send(()).expect("release candidate validator");
    cx.run_until_parked();
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "worker must skip a symlinked first candidate and stage the later descriptor-safe file"
    );
}

#[gpui::test]
async fn mounted_html_data_uri_stays_encoded_until_the_retained_stage_worker(
    cx: &mut TestAppContext,
) {
    // Case-insensitive HTML attributes are common in browser pasteboards.
    // The callback may boundedly retain this representation, but it must not
    // lowercase the whole HTML string or base64-decode it before scheduling
    // the retained stage worker.
    let (_profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        ClipboardPayload::fixture_with_png_and_text("")
            .images
            .into_iter()
            .next()
            .expect("fixture png")
            .bytes,
    );
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    let html = format!("<IMG alt='保留原始大小写' src='DATA:IMAGE/PNG;BASE64,{encoded}'>");

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_clipboard_payload_for_test(
                    ClipboardPayload {
                        html: Some(html),
                        text: Some("不要把 data URI 退回成此文本".into()),
                        ..ClipboardPayload::default()
                    },
                    window,
                    shell_cx,
                )
                .expect("HTML paste callback must queue an encoded descriptor");
        });
    });
    cx.run_until_parked();
    draw_once(cx);
    assert!(
        view.read_with(cx, |shell, _| shell.resource_notice_for_test())
            .is_none(),
        "a data URI must not be parsed/decoded or downgraded to text in the callback"
    );

    release.send(()).expect("release HTML data-uri worker");
    cx.run_until_parked();
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "worker-side bounded decode must insert the image through the usual atomic transaction"
    );
}

#[gpui::test]
async fn mounted_lifecycle_switch_waits_for_a_gated_resource_stage_then_commits_it(
    cx: &mut TestAppContext,
) {
    // Staging is not an expendable picker callback.  It owns a saved document
    // selection and a descriptor-safe blob preparation that must either reach
    // the one note/resource commit or visibly block a lifecycle boundary. If
    // `flush` only looks at SaveState::Clean, CreateNote destroys this session
    // while the retained stage task is gated and silently loses the import.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let original_note_id = view.read_with(cx, |shell, app| {
        shell.image_flow_probe_for_test(app).note_id
    });
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture the active selection before the staged import");
            shell
                .complete_resource_picker_path(picker_png(&profile), window, shell_cx)
                .expect("schedule the retained stage worker");
        });
    });
    cx.run_until_parked();
    redraw(cx);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    assert_eq!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .note_id),
        original_note_id,
        "a lifecycle switch must stay on the stage-owning session instead of dropping its saved intent"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.save_error_for_test())
            .is_some_and(|message| message.contains("资源")),
        "the pending resource stage must surface a lifecycle blocker"
    );
    assert!(
        repository
            .load_note(&original_note_id)
            .expect("load gated note")
            .expect("note remains")
            .resource_ids
            .is_empty(),
        "a stage gate must publish neither metadata nor a note relation"
    );

    // The session remains editable while hashing/copying is off-thread. The
    // eventual resource commit must include this newer text rather than an
    // old picker-time snapshot.
    cx.simulate_input("stage 尚未完成时的正文");
    redraw(cx);

    release.send(()).expect("release the retained stage task");
    cx.run_until_parked();
    redraw(cx);
    let committed = repository
        .load_note(&original_note_id)
        .expect("load automatically continued resource commit")
        .expect("original note remains until a later switch");
    assert_eq!(
        committed.resource_ids.len(),
        1,
        "releasing the stage must automatically continue to the one durable resource snapshot"
    );
    assert!(
        committed.body_text.contains("stage 尚未完成时的正文"),
        "the durable resource snapshot must retain text typed while staging was gated"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.save_error_for_test())
            .is_none(),
        "the lifecycle blocker clears only after the resource transaction is durable"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    assert_ne!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .note_id),
        original_note_id,
        "the next lifecycle request proceeds only after the stage/commit barrier has cleared"
    );
}

#[gpui::test]
async fn mounted_external_organization_event_keeps_a_staged_resource_session_alive(
    cx: &mut TestAppContext,
) {
    // An external metadata writer can advance the active note while the
    // retained ResourceStageJob still owns a saved insert selection. The
    // bridge must keep that task/session intact and visibly block the event;
    // installing the new revision would discard the stage without another
    // chance to commit or report it.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let before = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_import_for_test(shell_cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture a saved resource insert selection");
            shell
                .complete_resource_picker_path(picker_png(&profile), window, shell_cx)
                .expect("schedule the real retained ResourceStageJob");
        });
    });
    cx.run_until_parked();
    redraw(cx);

    let tag = repository.create_tag("stage race tag").expect("create tag");
    repository
        .add_note_tag(&before.note_id, &tag.id)
        .expect("external metadata revision commits");
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);

    let after = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert_eq!(after.note_id, before.note_id);
    assert_eq!(
        after.session_entity_id, before.session_entity_id,
        "the external event must not remount away the stage-owning session"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.save_error_for_test())
            .is_some_and(|message| message.contains("资源")),
        "the lifecycle barrier must remain visible until the stage owner resolves"
    );
    assert!(
        repository
            .load_note(&before.note_id)
            .expect("load original note")
            .expect("active note remains")
            .resource_ids
            .is_empty(),
        "the blocked stage must not publish a partial resource relation"
    );

    // Do not leave a retained worker suspended after the mounted assertion.
    release.send(()).expect("release stage task");
    cx.run_until_parked();
}

#[gpui::test]
async fn mounted_reconciliation_lock_rejects_a_picker_completion_captured_before_an_organization_partial_commit(
    cx: &mut TestAppContext,
) {
    // The platform picker can outlive the UI action that opened it.  If an
    // organization mutation commits while its full candidate fails, the old
    // note session is deliberately retained but temporarily locked.  A late
    // picker completion must not turn the pre-lock tracked selection into a
    // stage/SQLite mutation of that stale revision.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let model_for_shell = model.clone();
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model_for_shell.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let before = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    let tag = repository
        .create_tag("资源选择期间的组织变更")
        .expect("create tag used by the committed action");

    let old_picker_token = view.update(cx, |shell, shell_cx| {
        shell
            .begin_resource_picker(shell_cx)
            .expect("capture a real picker insert intent before the mutation")
    });
    model.update(cx, |model, _| {
        model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::AddTagToSelectedNote(tag.id.clone()),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    assert!(
        cx.debug_bounds("native-editor-surface-recovery-locked-notice")
            .is_some(),
        "the committed-but-unreconciled action must lock the same session before picker completion"
    );

    let completion = cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_resource_picker_path_for_token_for_test(
                old_picker_token,
                picker_png(&profile),
                window,
                shell_cx,
            )
        })
    });
    let completion_error = completion.expect_err(
        "a picker completion captured before the recovery lock must be rejected at the production completion seam",
    );
    assert!(completion_error.contains("正在恢复"));
    assert!(!completion_error.contains("废纸篓"));

    cx.run_until_parked();
    redraw(cx);
    let after = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert_eq!(after.note_id, before.note_id);
    assert_eq!(after.session_entity_id, before.session_entity_id);
    assert!(
        !after.has_image_block,
        "rejected completion must not mutate the retained old editor"
    );
    let durable = repository
        .load_note(&before.note_id)
        .expect("load tagged note")
        .expect("note remains after partial reconciliation");
    assert!(
        durable.resource_ids.is_empty(),
        "a rejected late picker completion must not publish a note relation"
    );
    let connection = Connection::open(profile.path().join("library.sqlite")).expect("open sqlite");
    let resource_rows: i64 = connection
        .query_row("SELECT count(*) FROM resources", [], |row| row.get(0))
        .expect("count committed resource metadata");
    let relation_rows: i64 = connection
        .query_row("SELECT count(*) FROM note_resources", [], |row| row.get(0))
        .expect("count committed resource relations");
    let blob_rows: i64 = connection
        .query_row("SELECT count(*) FROM resource_blobs", [], |row| row.get(0))
        .expect("count committed blob rows");
    assert_eq!(
        (resource_rows, relation_rows, blob_rows),
        (0, 0, 0),
        "the locked completion must not even begin a visible resource publication"
    );
}

#[gpui::test]
async fn mounted_stale_native_picker_completion_cannot_consume_a_new_picker_after_reconciliation(
    cx: &mut TestAppContext,
) {
    // AppKit can return an old panel after recovery has mounted a fresh
    // session and the person has already opened another picker.  The old
    // callback must carry its original token: matching only the current
    // `pending_resource_insert` would insert the old path at the new saved
    // selection and make the second panel silently disappear.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let model_for_shell = model.clone();
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model_for_shell.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let tag = repository
        .create_tag("旧选择器回调")
        .expect("create tag used to enter reconciliation");
    let old_token = view.update(cx, |shell, shell_cx| {
        shell
            .begin_resource_picker(shell_cx)
            .expect("open first native picker")
    });
    model.update(cx, |model, _| {
        model.fail_next_refresh_for_test(app_lite_core::LibraryError::NotFound);
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::AddTagToSelectedNote(tag.id.clone()),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    redraw(cx);

    let new_token = view.update(cx, |shell, shell_cx| {
        shell
            .begin_resource_picker(shell_cx)
            .expect("recovered session opens a second picker")
    });
    let stale_completion = cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.complete_resource_picker_path_for_token_for_test(
                old_token,
                picker_png(&profile),
                window,
                shell_cx,
            )
        })
    });
    assert!(
        stale_completion
            .expect_err("old native callback must not consume the new picker intent")
            .contains("正在恢复"),
        "the late callback should explain why its own selection was cancelled"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .complete_resource_picker_path_for_token_for_test(
                    new_token,
                    picker_png(&profile),
                    window,
                    shell_cx,
                )
                .expect("the newer panel still owns its saved selection");
        });
    });
    cx.run_until_parked();
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "rejecting the old callback must leave the current picker usable"
    );
}

#[gpui::test]
async fn mounted_resource_commit_keeps_live_typing_while_the_sqlite_worker_is_gated(
    cx: &mut TestAppContext,
) {
    // The resource block is applied optimistically after its background
    // descriptor stage succeeds.  Deleting that optimistic/live rebase path
    // and restoring a commit fence makes the input below disappear (or be
    // rejected) when the gated SQLite worker completes.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    let path = picker_png(&profile);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("picker captures a tracked full Selection");
            shell
                .complete_resource_picker_path(path.clone(), window, shell_cx)
                .expect("completion schedules worker-owned staging and commit");
        });
    });
    // Stage finishes, the optimistic resource transaction is visible, and
    // only the SQLite half is stopped at its retained test gate.
    cx.run_until_parked();
    redraw(cx);
    let during_commit = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        during_commit.has_image_block,
        "the current editor gets the structural image before SQLite returns"
    );
    let surface = cx
        .debug_bounds("native-editor-surface")
        .expect("mounted shared editor surface");
    cx.simulate_click(surface.center(), gpui::Modifiers::default());
    cx.simulate_input("提交期间继续输入");
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app))
            .contains("提交期间继续输入"),
        "the real EntityInputHandler must retain typing while the resource commit is gated"
    );

    release
        .send(())
        .expect("release retained SQLite resource worker");
    cx.run_until_parked();
    redraw(cx);
    let after = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        after.has_image_block,
        "commit completion must not remove the optimistic block"
    );
    let body = view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app));
    assert!(
        body.contains("提交期间继续输入"),
        "an old prepared editor must never overwrite later text"
    );
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    let persisted = repository
        .load_note(&after.note_id)
        .expect("load current durable note")
        .expect("note remains visible");
    assert_eq!(
        persisted.resource_ids.len(),
        1,
        "one atomic note/resource commit survives"
    );
    assert!(
        persisted.body_text.contains("提交期间继续输入"),
        "the rebased follow-up edit must reach the durable snapshot rather than only the canvas"
    );
}

#[gpui::test]
async fn mounted_resource_commit_failure_rolls_back_only_the_optimistic_atom_and_keeps_later_text(
    cx: &mut TestAppContext,
) {
    // A resource import is one cross-store transaction.  The editor may show
    // the prevalidated atom while its worker owns SQLite, but a worker error
    // must remove that exact atom without restoring an old document over text
    // entered meanwhile.  Removing either the rollback or the session-owned
    // worker failure handoff leaves a dangling resource relation or loses the
    // Chinese text below.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let note_id = view.read_with(cx, |shell, app| {
        shell.image_flow_probe_for_test(app).note_id
    });
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    view.update(cx, |shell, shell_cx| {
        shell.fail_next_resource_commit_for_test("强制资源提交失败", shell_cx);
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture tracked selection before failed import");
            shell
                .complete_resource_picker_path(picker_png(&profile), window, shell_cx)
                .expect("schedule the real staged resource route");
        });
    });
    cx.run_until_parked();
    redraw(cx);
    let optimistic = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        optimistic.has_image_block,
        "the failure test must first exercise the optimistic atom"
    );
    let text_bounds = optimistic
        .text_block_bounds
        .expect("optimistic insertion retains an adjacent real text block");
    cx.simulate_click(
        gpui::point(text_bounds.left() + gpui::px(4.0), text_bounds.center().y),
        gpui::Modifiers::default(),
    );
    redraw(cx);
    cx.simulate_input("失败期间仍保留这段文字");
    redraw(cx);

    release.send(()).expect("release failed commit worker");
    cx.run_until_parked();
    redraw(cx);
    let after = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        !after.has_image_block,
        "failed cross-store commit must remove its one optimistic image atom"
    );
    assert!(
        view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app))
            .contains("失败期间仍保留这段文字"),
        "rollback must rebase on the live editor and never lose later input"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.resource_notice_for_test())
            .is_some_and(|notice| notice.contains("强制资源提交失败")),
        "the failed staged transaction must remain visibly recoverable"
    );
    let before_retry = repository
        .load_note(&note_id)
        .expect("load note after failed worker")
        .expect("note remains");
    assert!(
        before_retry.resource_ids.is_empty(),
        "a failed worker must expose neither a resource metadata row nor note relation"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    let durable = repository
        .load_note(&note_id)
        .expect("load manual-sync result")
        .expect("note persists after failure");
    assert!(durable.resource_ids.is_empty());
    assert!(
        durable.body_text.contains("失败期间仍保留这段文字"),
        "the next ordinary snapshot must persist surviving text without the failed resource"
    );
}

#[gpui::test]
async fn mounted_resource_commit_failure_at_default_right_caret_retries_without_losing_suffix_text(
    cx: &mut TestAppContext,
) {
    // `InsertImage` creates a right-side paragraph and leaves the real input
    // caret there. This is the common path: a person pastes an image and
    // immediately continues typing, without clicking a surviving left block.
    // If the worker fails, that right node disappears while unwinding the
    // optimistic insertion, so rollback must fail closed into the retained
    // staged retry path. Manual sync must actually consume that retry rather
    // than leave the session permanently failed or discard the suffix.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let note_id = view.read_with(cx, |shell, app| {
        shell.image_flow_probe_for_test(app).note_id
    });
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    view.update(cx, |shell, shell_cx| {
        shell.fail_next_resource_commit_for_test("默认右侧段资源提交失败", shell_cx);
    });

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture the default caret before image insertion");
            shell
                .complete_resource_picker_path(picker_png(&profile), window, shell_cx)
                .expect("schedule the real staged image transaction");
        });
    });
    cx.run_until_parked();
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "the optimistic atom must be visible before exercising its default right caret"
    );

    // No click or reselection: this is exactly the post-insert caret that the
    // shared surface left in the image's split-right paragraph.
    cx.simulate_input("紧跟图片右侧的后续中文");
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app))
            .contains("紧跟图片右侧的后续中文"),
        "the production EntityInputHandler must accept direct post-image typing"
    );

    release.send(()).expect("release failed resource worker");
    cx.run_until_parked();
    redraw(cx);
    assert!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "unsafe rollback must keep the optimistic atom as a retryable staged transaction"
    );
    assert!(
        view.read_with(cx, |shell, app| shell.resource_flow_body_text_for_test(app))
            .contains("紧跟图片右侧的后续中文"),
        "a failed worker must not lose text in the insertion-created right paragraph"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.resource_notice_for_test())
            .is_some_and(|notice| notice.contains("手动同步可重试")),
        "the staged retry must be visible rather than silently presenting an undurable image"
    );
    let failed = repository
        .load_note(&note_id)
        .expect("load note after injected worker error")
        .expect("note remains visible");
    assert!(
        failed.resource_ids.is_empty(),
        "the failed cross-store worker must not publish a partial relation"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    let retried = repository
        .load_note(&note_id)
        .expect("load manual retry result")
        .expect("note remains visible after retry");
    assert_eq!(
        retried.resource_ids.len(),
        1,
        "manual sync must consume the staged retry through one atomic resource snapshot"
    );
    assert!(
        retried.body_text.contains("紧跟图片右侧的后续中文"),
        "the retry snapshot must include the right-paragraph text typed while its first worker ran"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.save_error_for_test())
            .is_none(),
        "the successful retry must clear the lifecycle error it temporarily used to block the flush"
    );

    // The retry installed a real lifecycle barrier while its worker owned
    // SQLite. It must clear that exact barrier on success; otherwise the
    // next switch sees Clean + pending_flush and remains permanently blocked.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    assert_ne!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .note_id),
        note_id,
        "a successful staged retry must clear its lifecycle barrier so the next note switch proceeds"
    );
}

#[gpui::test]
async fn mounted_removed_failed_resource_manual_sync_releases_the_lifecycle_barrier(
    cx: &mut TestAppContext,
) {
    // A failed worker can retain a staged resource because the default
    // post-image caret created a suffix whose node cannot be safely inverted.
    // If the person then explicitly deletes that optimistic atom, Manual Sync
    // must snapshot only the surviving text and release the lifecycle token.
    // Leaving the token set produces Clean + permanently pending, so the next
    // note switch/close is incorrectly blocked despite no resource relation.
    cx.update(|app| crate::components::init(app));
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let note_id = view.read_with(cx, |shell, app| {
        shell.image_flow_probe_for_test(app).note_id
    });
    let release = view.update(cx, |shell, shell_cx| {
        shell.stall_next_resource_commit_for_test(shell_cx)
    });
    view.update(cx, |shell, shell_cx| {
        shell.fail_next_resource_commit_for_test("删除后的资源提交失败", shell_cx);
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture the pending image insertion");
            shell
                .complete_resource_picker_path(picker_png(&profile), window, shell_cx)
                .expect("schedule the real resource worker");
        });
    });
    cx.run_until_parked();
    redraw(cx);
    // Keep the default right paragraph alive so the first failure uses the
    // retained-stage branch rather than the easy inverse branch.
    cx.simulate_input("保留的后缀");
    redraw(cx);
    release.send(()).expect("release injected failed worker");
    cx.run_until_parked();
    redraw(cx);

    let image_bounds = view
        .read_with(cx, |shell, app| {
            shell.image_flow_probe_for_test(app).image_block_bounds
        })
        .expect("failed staged image remains visible for explicit removal");
    cx.simulate_click(image_bounds.center(), gpui::Modifiers::default());
    redraw(cx);
    cx.simulate_keystrokes("backspace");
    redraw(cx);
    assert!(
        !view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .has_image_block),
        "the explicit atomic deletion must remove the retryable image before sync"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::ManualSync, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);

    let persisted = repository
        .load_note(&note_id)
        .expect("load compacted note")
        .expect("note remains");
    assert!(persisted.resource_ids.is_empty());
    assert!(persisted.body_text.contains("保留的后缀"));
    assert!(
        repository
            .latest_edit_journal(&note_id)
            .expect("read journal after lifecycle snapshot")
            .is_none(),
        "the normal snapshot must compact its journal after discarded stage"
    );
    assert!(
        view.read_with(cx, |shell, _| shell.save_error_for_test())
            .is_none(),
        "once the ordinary snapshot is durable the old resource lifecycle barrier must clear"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    cx.run_until_parked();
    redraw(cx);
    assert_ne!(
        view.read_with(cx, |shell, app| shell
            .image_flow_probe_for_test(app)
            .note_id),
        note_id,
        "a completed discard snapshot must no longer block the next lifecycle boundary"
    );
}

#[gpui::test]
async fn second_image_import_keeps_existing_selected_thumbnail(cx: &mut TestAppContext) {
    // A newly inserted inline image is not an implicit "change cover" action.
    // Deleting the preserve-existing-thumbnail branch in the snapshot path
    // makes this fail by changing the selected card's thumbnail to the second
    // resource ID.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let path = picker_png(&profile);

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture first image insertion point");
            shell
                .complete_resource_picker_path(path.clone(), window, shell_cx)
                .expect("commit first image");
        });
    });
    redraw(cx);
    let first = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    let first_thumbnail = first
        .card_thumbnail_id
        .clone()
        .expect("first inline image becomes the default thumbnail");

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture second image insertion point");
            shell
                .complete_resource_picker_path(path.clone(), window, shell_cx)
                .expect("commit second image");
        });
    });
    redraw(cx);
    let second = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    let persisted = repository
        .load_note(&second.note_id)
        .expect("load saved note")
        .expect("note remains saved");
    assert_eq!(
        persisted.resource_ids.len(),
        2,
        "both inline images are associated"
    );
    assert_ne!(
        persisted.resource_ids[1], first_thumbnail,
        "the second image is a different durable resource even for identical bytes"
    );
    assert_eq!(
        second.card_thumbnail_id,
        Some(first_thumbnail),
        "adding a second inline image must not silently replace the existing card cover"
    );
}

#[gpui::test]
async fn mounted_attachment_picker_paints_a_durable_card_without_reopening_note(
    cx: &mut TestAppContext,
) {
    // This is deliberately the picker completion path, not a fabricated
    // document transaction. It catches a resource relation that committed
    // while the already-mounted surface remained a blank paragraph until a
    // later SelectNote/reopen.
    let (profile, repository) = repository();
    let model = cx.new({
        let repository = Arc::clone(&repository);
        move |_| AppModel::open(repository).expect("open real app model")
    });
    let clock = Arc::new(ManualSaveClock::default());
    let (view, cx) = cx.add_window_view(move |window, cx| {
        LibraryShell::new_with_save_clock(model.clone(), None, clock, window, cx)
    });
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(AppAction::CreateNote, window, shell_cx);
        });
    });
    redraw(cx);
    let path = picker_pdf(&profile);

    crate::native_editor::render::reset_test_render_observations();
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .begin_resource_picker(shell_cx)
                .expect("capture attachment insertion point");
            shell
                .complete_resource_picker_path(path.clone(), window, shell_cx)
                .expect("picker completion persists attachment and current card");
        });
    });
    cx.run_until_parked();
    draw_once(cx);
    let probe = view.read_with(cx, |shell, app| shell.image_flow_probe_for_test(app));
    assert!(
        probe.has_attachment_card,
        "same frame must expose attachment card"
    );
    assert!(
        crate::native_editor::render::test_attachment_card_paints() > 0,
        "the current shared canvas must actually paint the attachment card"
    );
    let resource_id = probe
        .attachment_resource_id
        .clone()
        .expect("attachment resource is part of the mounted document");
    assert_eq!(
        probe.attachment_size,
        Some(std::fs::metadata(&path).unwrap().len())
    );
    let saved = repository
        .load_note(&probe.note_id)
        .expect("load committed attachment note")
        .expect("note exists");
    assert_eq!(saved.resource_ids, vec![resource_id]);
    assert!(
        saved
            .body_html
            .contains("data-joplin-lite-block-attachment"),
        "attachment stays structural canonical body data rather than UI bytes"
    );
}

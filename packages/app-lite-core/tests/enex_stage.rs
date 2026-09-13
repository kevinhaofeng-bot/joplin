use std::{
    fs,
    io::Write,
    sync::{Arc, atomic::AtomicBool},
};

use app_lite_core::{
    EnexStageError, LibraryRepository, scan_enex_file, stage_enex_file, stage_enex_file_with_cancel,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::Md5;
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, tempdir};

fn archive(xml: &str) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(xml.as_bytes()).unwrap();
    file
}

fn resource(bytes: &[u8], mime: &str, filename: &str) -> String {
    format!(
        "<resource><data encoding=\"base64\">{}</data><mime>{mime}</mime><resource-attributes><file-name>{filename}</file-name></resource-attributes></resource>",
        STANDARD.encode(bytes)
    )
}

#[test]
fn stages_two_real_notes_and_reopens_with_exact_content_relations_and_no_outbox() {
    // Mutation caught: global MD5 resolution, omitted PDF/order/audit, lost
    // timestamps or tags, and ordinary create operations leaking to sync.
    let image = b"image bytes";
    let pdf = b"%PDF-1.7 fixture";
    let hash = format!("{:x}", Md5::digest(image));
    let first_enml = format!(
        "<en-note><div>前<en-media hash=\"{hash}\" type=\"image/png\"/>后</div><ul><li><en-todo checked=\"true\"/>已办</li><li><en-todo checked=\"false\"/>待办</li></ul><div><a href=\"https://example.com/x\">链接</a></div><div><en-media hash=\"{}\" type=\"application/pdf\"/></div></en-note>",
        format!("{:x}", Md5::digest(pdf))
    );
    let second_enml =
        format!("<en-note><div><en-media hash=\"{hash}\" type=\"image/png\"/></div></en-note>");
    let xml = format!(
        "<en-export><note><title>中文清单</title><created>20260913T010203Z</created><updated>20260913T040506Z</updated><tag>收件箱</tag><tag>收件箱</tag><content><![CDATA[{first_enml}]]></content>{}{}</note><note><title>独立图片</title><created>20260912T010203Z</created><updated>20260912T040506Z</updated><tag>图片</tag><content><![CDATA[{second_enml}]]></content>{}</note></en-export>",
        resource(image, "image/png", "图.png"),
        resource(pdf, "application/pdf", "附件.pdf"),
        resource(image, "image/png", "第二张.png"),
    );
    let source = archive(&xml);
    let parent = tempdir().unwrap();
    let stage = stage_enex_file(source.path(), parent.path()).unwrap();
    assert!(
        stage
            .profile_path()
            .starts_with(fs::canonicalize(parent.path()).unwrap())
    );
    assert_eq!(stage.report().notes.len(), 2);
    assert_eq!(stage.report().resources.len(), 3);
    assert_eq!(stage.report().tag_occurrences.len(), 3);
    assert_eq!(
        stage.report().tag_occurrences[0].destination_id,
        stage.report().tag_occurrences[1].destination_id
    );
    assert_ne!(
        stage.report().resources[0].destination_id,
        stage.report().resources[2].destination_id
    );
    assert_eq!(stage.report().pre_sync_outbox_rows, 0);
    assert!(stage.report().search_index_drained);

    let db = stage.profile_path().join("library.sqlite");
    let repo = LibraryRepository::open(&db).unwrap();
    let first = repo
        .load_note(&stage.report().notes[0].destination_id)
        .unwrap()
        .unwrap();
    let second = repo
        .load_note(&stage.report().notes[1].destination_id)
        .unwrap()
        .unwrap();
    assert_eq!(first.title, "中文清单");
    assert_eq!(first.created_time, 1_789_261_323_000);
    assert_eq!(first.updated_time, 1_789_272_306_000);
    assert_eq!(
        first.body_html,
        format!(
            "<p>前<img src=\":/{}\" alt=\"图.png\">后</p><ul data-type=\"checklist\"><li data-checked=\"true\">已办</li><li data-checked=\"false\">待办</li></ul><p><a href=\"https://example.com/x\">链接</a></p><a data-joplin-lite-block-attachment=\"true\" href=\":/{}\" data-resource-id=\"{}\" data-filename=\"附件.pdf\" data-media-type=\"application/pdf\">附件.pdf</a>",
            stage.report().resources[0].destination_id.as_str(),
            stage.report().resources[1].destination_id.as_str(),
            stage.report().resources[1].destination_id.as_str()
        )
    );
    assert_eq!(first.body_text, "前图.png后\n已办\n待办\n链接\n附件.pdf");
    assert_eq!(
        first.resource_ids,
        vec![
            stage.report().resources[0].destination_id.clone(),
            stage.report().resources[1].destination_id.clone()
        ]
    );
    assert_eq!(first.tag_ids.len(), 1);
    assert_eq!(
        second.resource_ids,
        vec![stage.report().resources[2].destination_id.clone()]
    );
    assert_eq!(repo.outbox_count().unwrap(), 0);
    for (index, bytes) in [
        (0, image.as_slice()),
        (1, pdf.as_slice()),
        (2, image.as_slice()),
    ] {
        let id = &stage.report().resources[index].destination_id;
        let (_, mut file) = repo.open_verified_resource_file(id).unwrap().unwrap();
        let mut digest = Sha256::new();
        std::io::copy(&mut file, &mut digest).unwrap();
        assert_eq!(
            format!("{:x}", digest.finalize()),
            format!("{:x}", Sha256::digest(bytes))
        );
    }
    let raw = rusqlite::Connection::open(&db).unwrap();
    let audit: String = raw
        .query_row(
            "SELECT raw_enml FROM enex_stage_audit WHERE note_ordinal=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(audit, first_enml);
    let second_audit: String = raw
        .query_row(
            "SELECT raw_enml FROM enex_stage_audit WHERE note_ordinal=2 AND note_id=?1",
            [second.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(second_audit, second_enml);
    for table in ["search_unicode", "search_trigram"] {
        let (title, body): (String, String) = raw
            .query_row(
                &format!("SELECT title, body FROM {table} WHERE note_id=?1"),
                [first.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, first.title);
        assert_eq!(body, first.body_text);
    }
    let thumb: String = raw
        .query_row(
            "SELECT selected_thumbnail_id FROM notes WHERE id=?1",
            [first.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(thumb, stage.report().resources[0].destination_id.as_str());
}

#[test]
fn rejects_unsupported_or_missing_attachments_without_touching_existing_profile() {
    // Mutation caught: partial publication or caller-selected output path.
    let parent = tempdir().unwrap();
    let sentinel = parent.path().join("live-profile");
    fs::create_dir(&sentinel).unwrap();
    fs::write(sentinel.join("keep.bin"), b"unchanged").unwrap();
    let unsupported = archive(
        "<en-export><note><title>x</title><content><![CDATA[<en-note><table><tr><td>x</td></tr></table></en-note>]]></content></note></en-export>",
    );
    assert!(matches!(
        stage_enex_file(unsupported.path(), parent.path()),
        Err(EnexStageError::Fidelity {
            note_ordinal: 1,
            path,
            ..
        }) if path == "/en-note/0"
    ));
    let missing = archive(
        "<en-export><note><title>x</title><content><![CDATA[<en-note><en-media hash=\"900150983cd24fb0d6963f7d28e17f72\" type=\"image/png\"/></en-note>]]></content></note></en-export>",
    );
    assert!(matches!(
        stage_enex_file(missing.path(), parent.path()),
        Err(EnexStageError::MissingResource {
            note_ordinal: 1,
            ..
        })
    ));
    let corrupt =
        archive("<en-export><note><resource><data>!!!!</data></resource></note></en-export>");
    assert!(stage_enex_file(corrupt.path(), parent.path()).is_err());
    assert_eq!(fs::read(sentinel.join("keep.bin")).unwrap(), b"unchanged");
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[test]
fn second_note_fidelity_failure_discards_previously_staged_note_and_attachment() {
    // Mutation caught: an error after a successful note/resource leaves a
    // partially populated stage child or changes a sibling profile.
    let parent = tempdir().unwrap();
    let sentinel = parent.path().join("live-profile");
    fs::create_dir(&sentinel).unwrap();
    fs::write(sentinel.join("keep.bin"), b"unchanged").unwrap();
    let bytes = b"first-note-attachment";
    let hash = format!("{:x}", Md5::digest(bytes));
    let xml = format!(
        "<en-export><note><title>已写入的笔记</title><content><![CDATA[<en-note><div>第一条<en-media hash=\"{hash}\" type=\"image/png\"/></div></en-note>]]></content>{}</note><note><title>失败的笔记</title><content><![CDATA[<en-note><table><tr><td>不支持</td></tr></table></en-note>]]></content></note></en-export>",
        resource(bytes, "image/png", "first.png"),
    );
    let source = archive(&xml);
    let scanned = scan_enex_file(source.path()).unwrap();
    assert_eq!(scanned.notes.len(), 2);
    assert_eq!(scanned.resources.len(), 1);
    assert!(matches!(
        stage_enex_file(source.path(), parent.path()),
        Err(EnexStageError::Fidelity {
            note_ordinal: 2,
            path,
            ..
        }) if path == "/en-note/0"
    ));
    assert_eq!(fs::read(sentinel.join("keep.bin")).unwrap(), b"unchanged");
    let entries = fs::read_dir(parent.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, vec![sentinel.file_name().unwrap()]);
}

#[test]
fn streams_attachment_larger_than_metadata_cap_to_owned_profile() {
    // Mutation caught: collecting <data> through the 16 KiB metadata buffer.
    let bytes = vec![b'x'; 256 * 1024];
    let xml = format!(
        "<en-export><note><title>大附件</title><content><![CDATA[<en-note><div>正文</div></en-note>]]></content>{}</note></en-export>",
        resource(&bytes, "application/pdf", "大附件.pdf")
    );
    let source = archive(&xml);
    let scanned = scan_enex_file(source.path()).unwrap();
    assert_eq!(scanned.resources[0].filename, "大附件.pdf");
    assert_eq!(scanned.resources[0].mime, "application/pdf");
    let parent = tempdir().unwrap();
    let stage = stage_enex_file(source.path(), parent.path()).unwrap();
    assert_eq!(stage.report().resources[0].byte_count, bytes.len());
    let repo = LibraryRepository::open(stage.profile_path().join("library.sqlite")).unwrap();
    let note = repo
        .load_note(&stage.report().notes[0].destination_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        note.resource_ids,
        vec![stage.report().resources[0].destination_id.clone()]
    );
    assert!(note.body_html.contains("大附件.pdf"), "{}", note.body_html);
    let (_, file) = repo
        .open_verified_resource_file(&stage.report().resources[0].destination_id)
        .unwrap()
        .unwrap();
    assert_eq!(file.metadata().unwrap().len(), bytes.len() as u64);
}

#[test]
fn stages_a_twenty_two_mib_resource_without_a_full_base64_buffer() {
    // Mutation caught: scanner-only support without a bounded ingestion path.
    let mut source = NamedTempFile::new().unwrap();
    source.write_all("<en-export><note><title>大文件</title><content><![CDATA[<en-note><div>可读</div></en-note>]]></content><resource><data encoding=\"base64\">".as_bytes()).unwrap();
    let encoded = b"QUFB".repeat(16 * 1024);
    for _ in 0..469 {
        source.write_all(&encoded).unwrap();
    }
    source.write_all(&b"QUFB".repeat(5_461)).unwrap();
    source.write_all(b"QQ==").unwrap();
    source.write_all(b"</data><mime>application/pdf</mime><resource-attributes><file-name>large.pdf</file-name></resource-attributes></resource></note></en-export>").unwrap();
    let parent = tempdir().unwrap();
    let stage = stage_enex_file(source.path(), parent.path()).unwrap();
    assert_eq!(stage.report().resources[0].byte_count, 22 * 1024 * 1024);
    let repo = LibraryRepository::open(stage.profile_path().join("library.sqlite")).unwrap();
    let (metadata, mut file) = repo
        .open_verified_resource_file(&stage.report().resources[0].destination_id)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.mime, "application/pdf");
    assert_eq!(metadata.title, "large.pdf");
    let mut digest = Sha256::new();
    let copied = std::io::copy(&mut file, &mut digest).unwrap();
    assert_eq!(copied, 22 * 1024 * 1024);
    assert_eq!(
        format!("{:x}", digest.finalize()),
        stage.report().resources[0].sha256
    );
}

#[test]
fn invalid_calendar_dates_block_stage_instead_of_rolling_into_next_month() {
    // Mutation caught: a numerically shaped but impossible date silently
    // changing note provenance during staging.
    let source = archive(
        "<en-export><note><title>日期</title><created>20260229T010203Z</created><updated>20260229T010203Z</updated><content><![CDATA[<en-note><div>内容</div></en-note>]]></content></note></en-export>",
    );
    let parent = tempdir().unwrap();
    assert!(matches!(
        stage_enex_file(source.path(), parent.path()),
        Err(EnexStageError::InvalidDate {
            note_ordinal: 1,
            ..
        })
    ));
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[test]
fn existing_live_profile_is_rejected_as_parent_and_cancellation_leaves_no_child() {
    // Mutation caught: a selected live profile being used as the stage parent.
    let source = archive(
        "<en-export><note><title>x</title><content><![CDATA[<en-note><div>x</div></en-note>]]></content></note></en-export>",
    );
    let live = tempdir().unwrap();
    drop(LibraryRepository::open(live.path().join("library.sqlite")).unwrap());
    let before = fs::read(live.path().join("library.sqlite")).unwrap();
    let entries = fs::read_dir(live.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<Vec<_>>();
    assert!(matches!(
        stage_enex_file(source.path(), live.path()),
        Err(EnexStageError::InvalidStagingParent)
    ));
    assert_eq!(
        fs::read(live.path().join("library.sqlite")).unwrap(),
        before
    );
    assert_eq!(
        fs::read_dir(live.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>(),
        entries
    );
    let parent = tempdir().unwrap();
    assert!(matches!(
        stage_enex_file_with_cancel(
            source.path(),
            parent.path(),
            Arc::new(AtomicBool::new(true))
        ),
        Err(EnexStageError::Cancelled)
    ));
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
}

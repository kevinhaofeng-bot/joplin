use std::{
    fs::{self, File},
    io::Cursor,
};

use app_lite_core::{JexStageError, LibraryRepository, stage_jex_file};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use tempfile::{NamedTempFile, TempPath, tempdir};

const MD: &str = "11111111111111111111111111111111";
const HTML: &str = "22222222222222222222222222222222";
const OTHER: &str = "33333333333333333333333333333333";

fn append(builder: &mut Builder<File>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

fn archive(entries: impl FnOnce(&mut Builder<File>)) -> TempPath {
    let path = NamedTempFile::new().unwrap().into_temp_path();
    let mut builder = Builder::new(File::create(&path).unwrap());
    entries(&mut builder);
    builder.finish().unwrap();
    path
}

fn note(id: &str, title: &str, body: &str, markup: i64, parent: &str) -> String {
    format!(
        "{title}\n\n{body}\n\nid: {id}\ntype_: 1\nparent_id: {parent}\nmarkup_language: {markup}\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: 2023-11-14T22:13:23.000Z\nuser_updated_time: 2023-11-14T22:13:24.000Z\n"
    )
}

fn listing(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn exporter_defaults(note: String) -> String {
    note.replace(
        "type_: 1\n",
        "is_conflict: 0\nlatitude: 0.00000000\nlongitude: 0.00000000\naltitude: 0.0000\nauthor: \nsource_url: \nis_todo: 0\ntodo_due: 0\ntodo_completed: 0\nsource: \nsource_application: \napplication_data: \norder: 0\ndeleted_time: 0\nencryption_applied: 0\nencryption_cipher_text: \nmaster_key_id: \nshare_id: \nis_shared: 0\nis_locked: 0\nextracted_resource_ids: \nconflict_original_id: \nuser_data: \ntype_: 1\n",
    )
}

#[test]
fn known_joplin_note_defaults_stage_but_trash_or_conflict_state_remains_refused() {
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let ordinary = exporter_defaults(note(MD, "默认字段", "正文", 1, ""));
    let source = archive(|tar| append(tar, &format!("{MD}.md"), ordinary.as_bytes()));
    let staged = stage_jex_file(&source, parent.path()).unwrap();
    let profile = staged.profile_path().to_path_buf();
    assert_eq!(staged.report().notes.len(), 1);
    let mapped = &staged.report().notes[0];
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let stored = repo.load_note(&mapped.destination_id).unwrap().unwrap();
    assert_eq!(stored.title, "默认字段");
    assert_eq!(stored.body_text, "正文");
    let db = Connection::open(profile.join("library.sqlite")).unwrap();
    let raw: Vec<u8> = db
        .query_row(
            "SELECT raw_item_bytes FROM jex_stage_note_audit WHERE source_id=?1",
            [MD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, ordinary.as_bytes());
    assert_eq!(mapped.raw_sha256, format!("{:x}", Sha256::digest(&raw)));
    drop(db);
    drop(repo);
    drop(staged);
    assert!(!profile.exists());

    // Since task 2 such metadata is retained in the raw audit and counted
    // (see real_note_metadata_is_retained_in_the_raw_audit_and_reported_not_rejected);
    // trash/conflict state still refuses.
    for nondefault in [
        ordinary.replace("deleted_time: 0\n", "deleted_time: 1\n"),
        ordinary.replace("is_conflict: 0\n", "is_conflict: 1\n"),
    ] {
        let source = archive(|tar| append(tar, &format!("{MD}.md"), nondefault.as_bytes()));
        assert!(matches!(
            stage_jex_file(&source, parent.path()),
            Err(JexStageError::UnsupportedNote { .. })
        ));
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")]
        );
    }

    // A nonempty whitespace-only author is real Joplin source content, not
    // the empty exporter default; a disguised key is not canonical either.
    for noncanonical in [
        ordinary.replace("author: \n", "author:  \n"),
        ordinary.replace("author: \n", "author : \n"),
    ] {
        let source = archive(|tar| append(tar, &format!("{MD}.md"), noncanonical.as_bytes()));
        assert!(matches!(
            stage_jex_file(&source, parent.path()),
            Err(JexStageError::UnsupportedNote { .. })
        ));
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")]
        );
    }
}

#[test]
fn stages_two_real_notes_then_reopens_audited_searchable_zero_outbox_profile() {
    // Mutation caught: returning a scan report, using a source-only spool DB,
    // losing raw bytes/user times, or trusting in-memory state without reopen.
    let md = note(MD, "中文笔记", "# 标题\n正文 **粗**", 1, "").replace('\n', "\r\n");
    let html = note(HTML, "网页笔记", "<p>前<strong>粗</strong>后</p>", 2, "");
    let source = archive(|tar| {
        append(tar, &format!("{HTML}.md"), html.as_bytes());
        append(tar, &format!("{MD}.md"), md.as_bytes());
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let staged = stage_jex_file(&source, parent.path()).unwrap();
    let profile = staged.profile_path().to_path_buf();
    assert!(profile.starts_with(fs::canonicalize(parent.path()).unwrap()));
    assert!(profile.join("library.sqlite").is_file());
    assert_eq!(staged.report().preflight_counts.notes, 2);
    assert_eq!(staged.report().notes.len(), 2);
    assert_eq!(staged.report().verified_sync_outbox_rows, 0);
    assert!(staged.report().search_index_drained);

    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let db = Connection::open(profile.join("library.sqlite")).unwrap();
    assert_eq!(
        db.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get(0))
            .unwrap(),
        0
    );
    for (id, raw, expected_html, expected_search, markup) in [
        (
            MD,
            md.as_bytes(),
            "<h1>标题</h1><p>正文 <strong>粗</strong></p>",
            "标题\n正文 粗",
            1,
        ),
        (
            HTML,
            html.as_bytes(),
            "<p>前<strong>粗</strong>后</p>",
            "前粗后",
            2,
        ),
    ] {
        let mapped = staged
            .report()
            .notes
            .iter()
            .find(|entry| entry.source_id == id)
            .unwrap();
        let note = repo.load_note(&mapped.destination_id).unwrap().unwrap();
        assert_eq!(
            note.title,
            if id == MD {
                "中文笔记"
            } else {
                "网页笔记"
            }
        );
        assert_eq!(note.body_html, expected_html);
        assert_eq!(note.body_text, expected_search);
        assert_eq!(note.created_time, 1700000003000);
        assert_eq!(note.updated_time, 1700000004000);
        assert!(note.resource_ids.is_empty());
        assert!(note.tag_ids.is_empty());
        let audit: (Vec<u8>, Vec<u8>, i64, i64, i64, i64, i64) = db.query_row(
            "SELECT raw_item_bytes,raw_body_bytes,markup_language,created_time,updated_time,user_created_time,user_updated_time FROM jex_stage_note_audit WHERE source_id=?1 AND note_id=?2",
            params![id, note.id.as_str()],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        ).unwrap();
        assert_eq!(audit.0, raw);
        assert_eq!(
            audit.1,
            if id == MD {
                "# 标题\r\n正文 **粗**".as_bytes()
            } else {
                "<p>前<strong>粗</strong>后</p>".as_bytes()
            }
        );
        assert_eq!(audit.2, markup);
        assert_eq!(
            (audit.3, audit.4, audit.5, audit.6),
            (1700000001000, 1700000002000, 1700000003000, 1700000004000)
        );
        assert_eq!(mapped.raw_sha256, format!("{:x}", Sha256::digest(raw)));
        for table in ["search_unicode", "search_trigram"] {
            let body: String = db
                .query_row(
                    &format!("SELECT body FROM {table} WHERE note_id=?1"),
                    [note.id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(body, expected_search);
        }
    }
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM sync_outbox", [], |r| r.get(0))
            .unwrap(),
        0
    );
    assert!(!repo.has_pending_search_jobs().unwrap());
    drop(db);
    drop(repo);
    drop(staged);
    assert!(!profile.exists());
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
    assert_eq!(
        fs::read(parent.path().join("sentinel.bin")).unwrap(),
        b"keep"
    );
}

#[test]
fn blocks_invalid_folder_and_demotes_mismatched_resource_without_leaking_a_profile() {
    // Mutation caught: silently accepting malformed folder or resource bytes
    // whose signature contradicts declared resource MIME.
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let cases = [
        (2, format!("笔记本\n\nid: {OTHER}\ntype_: 2\n")),
        (
            4,
            format!("图.png\n\nid: {OTHER}\ntype_: 4\nmime: image/png\nfile_extension: png\n"),
        ),
    ];
    for (kind, metadata) in cases {
        let source = archive(|tar| {
            append(
                tar,
                &format!("{MD}.md"),
                note(MD, "笔记", "正文", 1, "").as_bytes(),
            );
            append(tar, &format!("{OTHER}.md"), metadata.as_bytes());
            if kind == 4 {
                append(tar, &format!("resources/{OTHER}.png"), b"physical-resource");
            }
        });
        if kind == 4 {
            // Since task 2 bytes contradicting a verifiable MIME are kept as
            // a generic attachment (never shown as an image) and reported.
            let staged = stage_jex_file(&source, parent.path()).unwrap();
            assert_eq!(
                staged.report().resources[0].mime,
                "application/octet-stream"
            );
            assert_eq!(staged.report().normalized_resources.len(), 1);
            drop(staged);
        } else {
            let error = stage_jex_file(&source, parent.path()).unwrap_err();
            assert!(
                matches!(error, JexStageError::UnsupportedFolder { .. }),
                "kind {kind}: {error:?}"
            );
        }
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")]
        );
    }
    assert_eq!(
        fs::read(parent.path().join("sentinel.bin")).unwrap(),
        b"keep"
    );
}

#[test]
fn malformed_or_empty_joplin_utc_time_blocks_and_cleans_after_first_note() {
    let first = note(MD, "先成功", "正文", 1, "");
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    for invalid in [
        "",
        "1700000004000",
        "2023-02-29T22:13:24.000Z",
        "2023-11-14T22:13:24.000+00:00",
    ] {
        let second = note(HTML, "坏时间", "正文", 1, "").replace(
            "user_updated_time: 2023-11-14T22:13:24.000Z",
            &format!("user_updated_time: {invalid}"),
        );
        let source = archive(|tar| {
            append(tar, &format!("{MD}.md"), first.as_bytes());
            append(tar, &format!("{HTML}.md"), second.as_bytes());
        });
        assert!(matches!(
            stage_jex_file(&source, parent.path()),
            Err(JexStageError::UnsupportedNote { .. })
        ));
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")]
        );
        assert_eq!(
            fs::read(parent.path().join("sentinel.bin")).unwrap(),
            b"keep"
        );
    }
}

#[test]
fn failed_second_note_cleans_only_owned_children_and_existing_profile_parent_is_rejected() {
    let first = note(MD, "先成功", "正文", 1, "");
    let second = format!(
        "{}deleted_time: 1700000000000\n",
        note(HTML, "后失败", "后失败正文", 1, "",)
    );
    let source = archive(|tar| {
        append(tar, &format!("{MD}.md"), first.as_bytes());
        append(tar, &format!("{HTML}.md"), second.as_bytes());
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    assert!(matches!(
        stage_jex_file(&source, parent.path()),
        Err(JexStageError::UnsupportedNote { .. })
    ));
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
    assert_eq!(
        fs::read(parent.path().join("sentinel.bin")).unwrap(),
        b"keep"
    );

    let live = tempdir().unwrap();
    fs::write(live.path().join("library.sqlite"), b"live-sentinel").unwrap();
    assert!(matches!(
        stage_jex_file(&source, live.path()),
        Err(JexStageError::Prepare(_))
    ));
    assert_eq!(
        fs::read(live.path().join("library.sqlite")).unwrap(),
        b"live-sentinel"
    );
}

#[test]
fn unsupported_markdown_degrades_to_readable_source_text_with_report() {
    // Plan task 2: a table/raw HTML note imports as readable text and is
    // reported, instead of blocking every other note in the library.
    let table = note(
        HTML,
        "表格",
        "|A|B|\n|-|-|\n|1|<span class=\"x\">2</span>|",
        1,
        "",
    );
    let plain = note(MD, "普通", "正文", 1, "");
    let source = archive(|tar| {
        append(tar, &format!("{MD}.md"), plain.as_bytes());
        append(tar, &format!("{HTML}.md"), table.as_bytes());
    });
    let parent = tempdir().unwrap();
    let stage = stage_jex_file(&source, parent.path()).unwrap();
    let report = stage.report();
    assert_eq!(report.notes.len(), 2);
    assert_eq!(report.degraded_notes.len(), 1);
    assert_eq!(report.degraded_notes[0].source_id, HTML);
    let repo = LibraryRepository::open(stage.profile_path().join("library.sqlite")).unwrap();
    let staged = report.notes.iter().find(|n| n.source_id == HTML).unwrap();
    let body = repo
        .load_note(&staged.destination_id)
        .unwrap()
        .unwrap()
        .body_text;
    for visible in ["|A|B|", "|1|<span class=\"x\">2</span>|"] {
        assert!(body.contains(visible), "{visible} in {body:?}");
    }
}

#[test]
fn real_note_metadata_is_retained_in_the_raw_audit_and_reported_not_rejected() {
    // Found in the user's export: 1665/1666 notes carry provenance, order,
    // location, author, source URL or to-do fields the schema has no column
    // for. They import; the fields stay in the raw audit and are counted.
    let ordinary = exporter_defaults(note(MD, "网页剪藏", "正文", 1, ""));
    let rich = ordinary
        .replace("source_url: \n", "source_url: https://example.org/a\n")
        .replace("author: \n", "author: 某人\n")
        .replace("latitude: 0.00000000", "latitude: 35.68000000")
        .replace("order: 0\n", "order: -62167248343000\n")
        .replace("is_todo: 0\n", "is_todo: 1\n")
        .replace("todo_completed: 0\n", "todo_completed: 1659891183430\n")
        .replace(
            "source_application: \n",
            "source_application: net.cozic.joplin-desktop\n",
        );
    let source = archive(|tar| append(tar, &format!("{MD}.md"), rich.as_bytes()));
    let parent = tempdir().unwrap();
    let stage = stage_jex_file(&source, parent.path()).unwrap();
    let fields = &stage.report().retained_only_note_fields;
    for key in [
        "source_url",
        "author",
        "latitude",
        "order",
        "is_todo",
        "todo_completed",
        "source_application",
    ] {
        assert_eq!(fields.get(key), Some(&1), "{key} in {fields:?}");
    }
    let db = Connection::open(stage.profile_path().join("library.sqlite")).unwrap();
    let raw: Vec<u8> = db
        .query_row(
            "SELECT raw_item_bytes FROM jex_stage_note_audit WHERE source_id=?1",
            [MD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, rich.as_bytes(), "raw item kept byte-for-byte");
    drop(db);
    drop(stage);

    // Trash/conflict state must not be silently resurrected as a live note.
    let trashed = ordinary.replace("deleted_time: 0\n", "deleted_time: 1700000000000\n");
    let source = archive(|tar| append(tar, &format!("{MD}.md"), trashed.as_bytes()));
    assert!(matches!(
        stage_jex_file(&source, parent.path()),
        Err(JexStageError::UnsupportedNote { .. })
    ));
}

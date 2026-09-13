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
        "is_conflict: 0\nlatitude: 0\nlongitude: 0\naltitude: 0\nauthor: \nsource_url: \nis_todo: 0\ntodo_due: 0\ntodo_completed: 0\nsource: \nsource_application: \napplication_data: \norder: 0\ndeleted_time: 0\nencryption_applied: 0\nencryption_cipher_text: \nmaster_key_id: \nshare_id: \nis_shared: 0\nis_locked: 0\nextracted_resource_ids: \nconflict_original_id: \nuser_data: \ntype_: 1\n",
    )
}

#[test]
fn known_joplin_note_defaults_stage_but_nondefault_source_url_remains_refused() {
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let ordinary = exporter_defaults(note(MD, "默认字段", "正文", 1, ""));
    let source = archive(|tar| append(tar, &format!("{MD}.md"), ordinary.as_bytes()));
    let staged = stage_jex_file(&source, parent.path()).unwrap();
    let profile = staged.profile_path().to_path_buf();
    assert_eq!(staged.report().notes.len(), 1);
    drop(staged);
    assert!(!profile.exists());

    let nondefault = ordinary.replace("source_url: \n", "source_url: https://example.org\n");
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
fn blocks_invalid_folder_and_mismatched_resource_without_returning_a_profile() {
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
        let error = stage_jex_file(&source, parent.path()).unwrap_err();
        assert!(
            if kind == 4 {
                matches!(error, JexStageError::UnsupportedResource { .. })
            } else {
                matches!(error, JexStageError::UnsupportedFolder { .. })
            },
            "kind {kind}: {error:?}"
        );
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
    let second = note(HTML, "后失败", "|A|B|\n|-|-|\n|1|2|", 1, "");
    let source = archive(|tar| {
        append(tar, &format!("{MD}.md"), first.as_bytes());
        append(tar, &format!("{HTML}.md"), second.as_bytes());
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    assert!(matches!(
        stage_jex_file(&source, parent.path()),
        Err(JexStageError::Fidelity(_))
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

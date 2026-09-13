use std::{
    fs::{self, File},
    io::Cursor,
};

use app_lite_core::{JexStageError, LibraryRepository, stage_jex_file};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use tempfile::{NamedTempFile, TempPath, tempdir};

const NOTE_A: &str = "11111111111111111111111111111111";
const NOTE_B: &str = "22222222222222222222222222222222";
const ROOT_LEAF: &str = "33333333333333333333333333333333";
const ROOT_STACK: &str = "44444444444444444444444444444444";
const CHILD: &str = "55555555555555555555555555555555";
const PDF: &str = "66666666666666666666666666666666";
const TAG_A: &str = "77777777777777777777777777777777";
const TAG_B: &str = "88888888888888888888888888888888";
const REL_A: &str = "99999999999999999999999999999999";
const REL_B: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REL_C: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn append(tar: &mut Builder<File>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

fn archive(entries: impl FnOnce(&mut Builder<File>)) -> TempPath {
    let path = NamedTempFile::new().unwrap().into_temp_path();
    let mut tar = Builder::new(File::create(&path).unwrap());
    entries(&mut tar);
    tar.finish().unwrap();
    path
}

fn note(id: &str, title: &str, body: &str, markup: i64, parent: &str) -> String {
    format!(
        "{title}\n\n{body}\n\nid: {id}\ntype_: 1\nparent_id: {parent}\nmarkup_language: {markup}\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: 2023-11-14T22:13:23.000Z\nuser_updated_time: 2023-11-14T22:13:24.000Z\n"
    )
}

fn folder(id: &str, title: &str, parent: &str) -> String {
    format!(
        "{title}\n\nid: {id}\ntype_: 2\nparent_id: {parent}\ncreated_time: 2022-12-31T23:59:59.000Z\nupdated_time: 2023-01-01T00:00:01.000Z\n"
    )
}

fn tag(id: &str, title: &str) -> String {
    format!(
        "{title}\n\nid: {id}\ntype_: 5\ncreated_time: 2022-12-31T23:59:59.000Z\nupdated_time: 2023-01-01T00:00:01.000Z\n"
    )
}

fn relation(id: &str, note_id: &str, tag_id: &str) -> String {
    format!(
        "id: {id}\ntype_: 6\nnote_id: {note_id}\ntag_id: {tag_id}\ncreated_time: 2023-01-02T00:00:00.000Z\nupdated_time: 2023-01-02T00:00:01.000Z\n"
    )
}

fn folder_exporter_defaults(id: &str, title: &str) -> String {
    format!(
        "{title}\n\nid: {id}\nparent_id: \ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: \nuser_updated_time: \ndeleted_time: 0\nencryption_applied: 0\nencryption_cipher_text: \nicon: \nis_shared: 0\nmaster_key_id: \nshare_id: \nuser_data: \ntype_: 2\n"
    )
}

fn tag_exporter_defaults(id: &str, title: &str) -> String {
    format!(
        "{title}\n\nid: {id}\nparent_id: \ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: \nuser_updated_time: \nencryption_applied: 0\nencryption_cipher_text: \nis_shared: 0\nuser_data: \ntype_: 5\n"
    )
}

fn relation_exporter_defaults(id: &str, note_id: &str, tag_id: &str) -> String {
    format!(
        "id: {id}\nnote_id: {note_id}\ntag_id: {tag_id}\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: \nuser_updated_time: \nencryption_applied: 0\nencryption_cipher_text: \nis_shared: 0\ntype_: 6\n"
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

#[test]
fn full_exporter_default_folder_tag_relation_stage_and_nondefaults_clean_up() {
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let source = archive(|tar| {
        append(
            tar,
            &format!("{ROOT_LEAF}.md"),
            folder_exporter_defaults(ROOT_LEAF, "资料").as_bytes(),
        );
        append(
            tar,
            &format!("{NOTE_A}.md"),
            note(NOTE_A, "笔记", "正文", 1, ROOT_LEAF).as_bytes(),
        );
        append(
            tar,
            &format!("{TAG_A}.md"),
            tag_exporter_defaults(TAG_A, "要务").as_bytes(),
        );
        append(
            tar,
            &format!("{REL_A}.md"),
            relation_exporter_defaults(REL_A, NOTE_A, TAG_A).as_bytes(),
        );
    });
    let staged = stage_jex_file(&source, parent.path()).unwrap();
    let path = staged.profile_path().to_path_buf();
    let db = Connection::open(path.join("library.sqlite")).unwrap();
    for table in [
        "notes",
        "tags",
        "note_tags",
        "jex_stage_folder_audit",
        "jex_stage_tag_audit",
        "jex_stage_relation_audit",
    ] {
        assert_eq!(
            db.query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            1,
            "{table}"
        );
    }
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM notebooks", [], |r| r.get(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM sync_outbox", [], |r| r.get(0))
            .unwrap(),
        0
    );
    drop(db);
    drop(staged);
    assert!(!path.exists());

    for (label, replace, expected) in [
        (
            "folder",
            ("is_shared: 0", "is_shared: 1"),
            "UnsupportedFolder",
        ),
        ("tag", ("user_data: ", "user_data: state"), "UnsupportedTag"),
        (
            "relation",
            ("is_shared: 0", "is_shared: 1"),
            "UnsupportedRelation",
        ),
    ] {
        let folder = folder_exporter_defaults(ROOT_LEAF, "资料");
        let tag = tag_exporter_defaults(TAG_A, "要务");
        let relation = relation_exporter_defaults(REL_A, NOTE_A, TAG_A);
        let (folder, tag, relation) = match label {
            "folder" => (folder.replacen(replace.0, replace.1, 1), tag, relation),
            "tag" => (folder, tag.replacen(replace.0, replace.1, 1), relation),
            _ => (folder, tag, relation.replacen(replace.0, replace.1, 1)),
        };
        let source = archive(|tar| {
            append(tar, &format!("{ROOT_LEAF}.md"), folder.as_bytes());
            append(
                tar,
                &format!("{NOTE_A}.md"),
                note(NOTE_A, "笔记", "正文", 1, ROOT_LEAF).as_bytes(),
            );
            append(tar, &format!("{TAG_A}.md"), tag.as_bytes());
            append(tar, &format!("{REL_A}.md"), relation.as_bytes());
        });
        let error = stage_jex_file(&source, parent.path()).unwrap_err();
        assert!(
            format!("{error:?}").contains(expected),
            "{label}: {error:?}"
        );
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")]
        );
    }

    // BaseItem.serialize_format writes a falsy timestamp as an empty value,
    // never as literal `0`; do not broaden that spelling by convenience.
    let source = archive(|tar| {
        append(
            tar,
            &format!("{ROOT_LEAF}.md"),
            folder_exporter_defaults(ROOT_LEAF, "资料")
                .replace("user_created_time: \n", "user_created_time: 0\n")
                .as_bytes(),
        );
        append(
            tar,
            &format!("{NOTE_A}.md"),
            note(NOTE_A, "笔记", "正文", 1, ROOT_LEAF).as_bytes(),
        );
    });
    assert!(matches!(
        stage_jex_file(&source, parent.path()),
        Err(JexStageError::UnsupportedFolder { .. })
    ));
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
}

#[test]
fn shuffled_tags_and_relations_reopen_with_exact_note_membership() {
    // Mutation caught: skipping tag/relation classes, losing source provenance,
    // assigning all tags to one note, or regressing folder/resource/body stage.
    let notes = [
        (
            NOTE_A,
            note(
                NOTE_A,
                "叶中笔记",
                &format!("正文\n\n[附件.pdf](:/{PDF})"),
                1,
                ROOT_LEAF,
            ),
        ),
        (
            NOTE_B,
            note(NOTE_B, "子中笔记", "<p>网页正文</p>", 2, CHILD),
        ),
    ];
    let tags = [(TAG_A, tag(TAG_A, "要务")), (TAG_B, tag(TAG_B, "归档"))];
    let relations = [
        (REL_A, relation(REL_A, NOTE_A, TAG_A)),
        (REL_B, relation(REL_B, NOTE_B, TAG_B)),
        (
            REL_C,
            relation(
                REL_C,
                &NOTE_A.to_ascii_uppercase(),
                &TAG_B.to_ascii_uppercase(),
            ),
        ),
    ];
    let pdf_meta =
        format!("附件.pdf\n\nid: {PDF}\ntype_: 4\nmime: application/pdf\nfile_extension: pdf\n");
    let pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n";
    let source = archive(|tar| {
        append(tar, &format!("{REL_C}.md"), relations[2].1.as_bytes());
        append(tar, &format!("{NOTE_B}.md"), notes[1].1.as_bytes());
        append(tar, &format!("{TAG_B}.md"), tags[1].1.as_bytes());
        append(
            tar,
            &format!("{CHILD}.md"),
            folder(CHILD, "合同", ROOT_STACK).as_bytes(),
        );
        append(tar, &format!("{PDF}.md"), pdf_meta.as_bytes());
        append(tar, &format!("resources/{PDF}.pdf"), pdf);
        append(
            tar,
            &format!("{ROOT_LEAF}.md"),
            folder(ROOT_LEAF, "根叶", "").as_bytes(),
        );
        append(tar, &format!("{REL_A}.md"), relations[0].1.as_bytes());
        append(tar, &format!("{TAG_A}.md"), tags[0].1.as_bytes());
        append(tar, &format!("{NOTE_A}.md"), notes[0].1.as_bytes());
        append(
            tar,
            &format!("{ROOT_STACK}.md"),
            folder(ROOT_STACK, "项目", "").as_bytes(),
        );
        append(tar, &format!("{REL_B}.md"), relations[1].1.as_bytes());
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let staged = stage_jex_file(&source, parent.path()).unwrap();
    let path = staged.profile_path().to_path_buf();
    let report = staged.report();
    assert_eq!(report.preflight_counts.tags, 2);
    assert_eq!(report.preflight_counts.note_tag_relations, 3);
    assert_eq!(report.tags.len(), 2);
    assert_eq!(report.relations.len(), 3);
    assert_eq!(report.verified_sync_outbox_rows, 0);
    assert!(report.search_index_drained);
    let db = Connection::open(path.join("library.sqlite")).unwrap();
    let repo = LibraryRepository::open(path.join("library.sqlite")).unwrap();
    for (table, count) in [
        ("tags", 2),
        ("note_tags", 3),
        ("jex_stage_tag_audit", 2),
        ("jex_stage_relation_audit", 3),
        ("notes", 2),
        ("stacks", 1),
        ("notebooks", 3),
        ("resources", 1),
        ("note_resources", 1),
        ("search_unicode", 2),
        ("search_trigram", 2),
        ("sync_outbox", 0),
    ] {
        assert_eq!(
            db.query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            count,
            "{table}"
        );
    }
    let tag_a_id: String = db
        .query_row("SELECT id FROM tags WHERE title='要务'", [], |r| r.get(0))
        .unwrap();
    let tag_b_id: String = db
        .query_row("SELECT id FROM tags WHERE title='归档'", [], |r| r.get(0))
        .unwrap();
    for (source_id, raw, dest) in [
        (TAG_A, tags[0].1.as_bytes(), &tag_a_id),
        (TAG_B, tags[1].1.as_bytes(), &tag_b_id),
    ] {
        let mapped = report
            .tags
            .iter()
            .find(|tag| tag.source_id == source_id)
            .unwrap();
        assert_eq!(mapped.destination_id.as_str(), dest);
        assert_eq!(mapped.raw_sha256, format!("{:x}", Sha256::digest(raw)));
        let (stored_raw, stored_dest, created, updated): (Vec<u8>,String,i64,i64) = db.query_row(
            "SELECT raw_item_bytes,tag_id,created_time,updated_time FROM jex_stage_tag_audit WHERE source_id=?1",
            [source_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).unwrap();
        assert_eq!(stored_raw, raw);
        assert_eq!(&stored_dest, dest);
        assert_eq!((created, updated), (1672531199000, 1672531201000));
        let actual_times: (i64, i64) = db
            .query_row(
                "SELECT created_time,updated_time FROM tags WHERE id=?1",
                [dest],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(actual_times, (created, updated));
    }
    for (source_id, raw) in &relations {
        let mapped = report
            .relations
            .iter()
            .find(|relation| relation.source_id == *source_id)
            .unwrap();
        assert_eq!(
            mapped.raw_sha256,
            format!("{:x}", Sha256::digest(raw.as_bytes()))
        );
        let (stored_raw, created, updated): (Vec<u8>,i64,i64) = db.query_row(
            "SELECT raw_item_bytes,created_time,updated_time FROM jex_stage_relation_audit WHERE source_id=?1",
            [source_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).unwrap();
        assert_eq!(stored_raw, raw.as_bytes());
        assert_eq!((created, updated), (1672617600000, 1672617601000));
    }
    let uppercase = report
        .relations
        .iter()
        .find(|relation| relation.source_id == REL_C)
        .unwrap();
    assert_eq!(uppercase.source_note_id, NOTE_A.to_ascii_uppercase());
    assert_eq!(uppercase.source_tag_id, TAG_B.to_ascii_uppercase());
    assert_eq!(uppercase.destination_tag_id.as_str(), tag_b_id);
    let note_a = repo
        .load_note(
            &report
                .notes
                .iter()
                .find(|n| n.source_id == NOTE_A)
                .unwrap()
                .destination_id,
        )
        .unwrap()
        .unwrap();
    let note_b = repo
        .load_note(
            &report
                .notes
                .iter()
                .find(|n| n.source_id == NOTE_B)
                .unwrap()
                .destination_id,
        )
        .unwrap()
        .unwrap();
    let mut statement = db.prepare("SELECT source_id,source_note_id,source_tag_id,note_id,tag_id FROM jex_stage_relation_audit ORDER BY source_id").unwrap();
    let provenance = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        provenance,
        vec![
            (
                REL_A.to_owned(),
                NOTE_A.to_owned(),
                TAG_A.to_owned(),
                note_a.id.as_str().to_owned(),
                tag_a_id.clone()
            ),
            (
                REL_B.to_owned(),
                NOTE_B.to_owned(),
                TAG_B.to_owned(),
                note_b.id.as_str().to_owned(),
                tag_b_id.clone()
            ),
            (
                REL_C.to_owned(),
                NOTE_A.to_ascii_uppercase(),
                TAG_B.to_ascii_uppercase(),
                note_a.id.as_str().to_owned(),
                tag_b_id.clone()
            ),
        ]
    );
    drop(statement);
    assert_eq!(
        note_a
            .tag_ids
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec![tag_a_id.as_str(), tag_b_id.as_str()]
    );
    assert_eq!(
        note_b
            .tag_ids
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec![tag_b_id.as_str()]
    );
    assert_eq!(
        (note_a.created_time, note_a.updated_time),
        (1700000003000, 1700000004000)
    );
    assert_eq!(
        (note_b.created_time, note_b.updated_time),
        (1700000003000, 1700000004000)
    );
    assert_eq!(
        note_a.resource_ids,
        vec![report.resources[0].destination_id.clone()]
    );
    assert_eq!(note_a.body_text, "正文\n附件.pdf");
    assert_eq!(note_b.body_html, "<p>网页正文</p>");
    assert_eq!(note_b.body_text, "网页正文");
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
    drop(repo);
    drop(db);
    drop(staged);
    assert!(!path.exists());
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
fn tag_title_and_relation_graph_conflicts_are_source_located_and_leave_no_child() {
    // Mutations caught: silent Tag.loadByTitle-like merge, idempotent duplicate
    // relation loss, dangling endpoint fallback, or discarded source fields.
    let cases: [(&str, Vec<(&str, String)>, Vec<(&str, String)>, bool, &str); 10] = [
        (
            "case-folded tag title",
            vec![(TAG_A, tag(TAG_A, "Ops")), (TAG_B, tag(TAG_B, "ops"))],
            vec![],
            true,
            TAG_B,
        ),
        (
            "duplicate relation pair",
            vec![(TAG_A, tag(TAG_A, "要务"))],
            vec![
                (REL_A, relation(REL_A, NOTE_A, TAG_A)),
                (
                    REL_B,
                    relation(
                        REL_B,
                        &NOTE_A.to_ascii_uppercase(),
                        &TAG_A.to_ascii_uppercase(),
                    ),
                ),
            ],
            false,
            REL_B,
        ),
        (
            "dangling note endpoint",
            vec![(TAG_A, tag(TAG_A, "要务"))],
            vec![(REL_A, relation(REL_A, NOTE_B, TAG_A))],
            false,
            REL_A,
        ),
        (
            "dangling tag endpoint",
            vec![(TAG_A, tag(TAG_A, "要务"))],
            vec![(REL_A, relation(REL_A, NOTE_A, TAG_B))],
            false,
            REL_A,
        ),
        (
            "tag body",
            vec![(
                TAG_A,
                tag(TAG_A, "要务").replacen("\n\nid:", "\n\n不应丢弃\n\nid:", 1),
            )],
            vec![],
            true,
            TAG_A,
        ),
        (
            "relation title",
            vec![(TAG_A, tag(TAG_A, "要务"))],
            vec![(
                REL_A,
                format!("不可丢弃\n\n{}", relation(REL_A, NOTE_A, TAG_A)),
            )],
            false,
            REL_A,
        ),
        (
            "invalid relation time",
            vec![(TAG_A, tag(TAG_A, "要务"))],
            vec![(
                REL_A,
                relation(REL_A, NOTE_A, TAG_A)
                    .replace("2023-01-02T00:00:01.000Z", "2023-02-29T00:00:01.000Z"),
            )],
            false,
            REL_A,
        ),
        (
            "shared tag metadata",
            vec![(
                TAG_A,
                tag(TAG_A, "要务").replace("type_: 5\n", "type_: 5\nis_shared: 1\n"),
            )],
            vec![],
            true,
            TAG_A,
        ),
        (
            "nonempty tag user_data",
            vec![(
                TAG_A,
                tag(TAG_A, "要务").replace("type_: 5\n", "type_: 5\nuser_data: 0\n"),
            )],
            vec![],
            true,
            TAG_A,
        ),
        (
            "nonempty tag parent",
            vec![(
                TAG_A,
                tag(TAG_A, "要务").replace("type_: 5\n", "type_: 5\nparent_id: 0\n"),
            )],
            vec![],
            true,
            TAG_A,
        ),
    ];
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    for (label, tags, relations, tag_error, source_id) in cases {
        let source = archive(|tar| {
            append(
                tar,
                &format!("{NOTE_A}.md"),
                note(NOTE_A, "笔记", "正文", 1, "").as_bytes(),
            );
            for (id, raw) in &tags {
                append(tar, &format!("{id}.md"), raw.as_bytes());
            }
            for (id, raw) in &relations {
                append(tar, &format!("{id}.md"), raw.as_bytes());
            }
        });
        let error = stage_jex_file(&source, parent.path()).unwrap_err();
        match error {
            JexStageError::UnsupportedTag {
                source_id: id,
                source_path,
                reason,
            } if tag_error => {
                assert_eq!(id, source_id, "{label}");
                assert_eq!(source_path, format!("{source_id}.md"), "{label}");
                assert!(!reason.is_empty(), "{label}");
            }
            JexStageError::UnsupportedRelation {
                source_id: id,
                source_path,
                reason,
            } if !tag_error => {
                assert_eq!(id, source_id, "{label}");
                assert_eq!(source_path, format!("{source_id}.md"), "{label}");
                assert!(!reason.is_empty(), "{label}");
            }
            other => panic!("{label}: {other:?}"),
        }
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")],
            "{label}"
        );
        assert_eq!(
            fs::read(parent.path().join("sentinel.bin")).unwrap(),
            b"keep",
            "{label}"
        );
    }
}

#[test]
fn later_note_failure_after_tag_and_resource_creation_cleans_only_owned_child() {
    let pdf_meta =
        format!("附件.pdf\n\nid: {PDF}\ntype_: 4\nmime: application/pdf\nfile_extension: pdf\n");
    let source = archive(|tar| {
        append(
            tar,
            &format!("{NOTE_A}.md"),
            note(NOTE_A, "先成功", &format!("[附件.pdf](:/{PDF})"), 1, "").as_bytes(),
        );
        append(
            tar,
            &format!("{NOTE_B}.md"),
            note(NOTE_B, "后失败", "<script>alert(1)</script>", 2, "").as_bytes(),
        );
        append(tar, &format!("{TAG_A}.md"), tag(TAG_A, "要务").as_bytes());
        append(tar, &format!("{PDF}.md"), pdf_meta.as_bytes());
        append(tar, &format!("resources/{PDF}.pdf"), b"%PDF-1.4\n%%EOF\n");
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
}

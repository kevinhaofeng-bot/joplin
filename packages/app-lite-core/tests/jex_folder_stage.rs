use std::{
    fs::{self, File},
    io::Cursor,
};

use app_lite_core::{JexFolderDestination, JexStageError, LibraryRepository, stage_jex_file};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use tempfile::{NamedTempFile, TempPath, tempdir};

const NOTE_LEAF: &str = "11111111111111111111111111111111";
const NOTE_CHILD: &str = "22222222222222222222222222222222";
const ROOT_LEAF: &str = "33333333333333333333333333333333";
const ROOT_STACK: &str = "44444444444444444444444444444444";
const CHILD: &str = "55555555555555555555555555555555";
const PDF: &str = "66666666666666666666666666666666";

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

fn listing(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn root_leaf_and_stack_child_notes_reopen_in_their_source_folders() {
    // Mutations caught: type-2 omission, flattening into default notebook,
    // mis-parenting a child, losing note/resource contents or source times.
    let leaf_note = note(
        NOTE_LEAF,
        "叶中笔记",
        &format!("正文\n\n[附件.pdf](:/{PDF})"),
        1,
        ROOT_LEAF,
    );
    let child_note = note(NOTE_CHILD, "子中笔记", "<p>网页正文</p>", 2, CHILD);
    let leaf = folder(ROOT_LEAF, "根叶", "");
    let stack = folder(ROOT_STACK, "项目", "");
    let child = folder(CHILD, "合同", ROOT_STACK);
    let pdf_meta =
        format!("附件.pdf\n\nid: {PDF}\ntype_: 4\nmime: application/pdf\nfile_extension: pdf\n");
    let pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n";
    let source = archive(|tar| {
        append(tar, &format!("{NOTE_CHILD}.md"), child_note.as_bytes());
        append(tar, &format!("{CHILD}.md"), child.as_bytes());
        append(tar, &format!("resources/{PDF}.pdf"), pdf);
        append(tar, &format!("{ROOT_LEAF}.md"), leaf.as_bytes());
        append(tar, &format!("{NOTE_LEAF}.md"), leaf_note.as_bytes());
        append(tar, &format!("{PDF}.md"), pdf_meta.as_bytes());
        append(tar, &format!("{ROOT_STACK}.md"), stack.as_bytes());
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let staged = stage_jex_file(&source, parent.path()).unwrap();
    let profile = staged.profile_path().to_path_buf();
    let report = staged.report();
    assert_eq!(report.preflight_counts.folders, 3);
    assert_eq!(report.folders.len(), 3);
    assert_eq!(report.notes.len(), 2);
    assert_eq!(report.resources.len(), 1);
    assert_eq!(report.verified_sync_outbox_rows, 0);
    assert!(report.search_index_drained);
    let db = Connection::open(profile.join("library.sqlite")).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
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
    for (table, expected) in [
        ("stacks", 1),
        ("notebooks", 3),
        ("notes", 2),
        ("resources", 1),
        ("note_resources", 1),
        ("jex_stage_folder_audit", 3),
        ("sync_outbox", 0),
    ] {
        assert_eq!(
            db.query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            expected,
            "{table}"
        );
    }
    let root_leaf_id: String = db
        .query_row("SELECT id FROM notebooks WHERE title='根叶'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let stack_id: String = db
        .query_row("SELECT id FROM stacks WHERE title='项目'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let child_row: (String, String) = db
        .query_row(
            "SELECT id,stack_id FROM notebooks WHERE title='合同'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(child_row.1, stack_id);
    for (source_id, source_parent, raw, expected_destination) in [
        (ROOT_LEAF, "", leaf.as_bytes(), "notebook"),
        (ROOT_STACK, "", stack.as_bytes(), "stack"),
        (CHILD, ROOT_STACK, child.as_bytes(), "notebook"),
    ] {
        let mapped = report
            .folders
            .iter()
            .find(|folder| folder.source_id == source_id)
            .unwrap();
        assert_eq!(mapped.source_parent_id, source_parent);
        assert_eq!(
            (mapped.created_time, mapped.updated_time),
            (1672531199000, 1672531201000)
        );
        let (audit_raw, audit_parent, kind, dest, created, updated): (Vec<u8>,String,String,String,i64,i64) = db.query_row(
            "SELECT raw_item_bytes,source_parent_id,destination_kind,destination_id,created_time,updated_time FROM jex_stage_folder_audit WHERE source_id=?1",
            [source_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
        assert_eq!(audit_raw, raw);
        assert_eq!(audit_parent, source_parent);
        assert_eq!(kind, expected_destination);
        assert_eq!((created, updated), (1672531199000, 1672531201000));
        match &mapped.destination {
            JexFolderDestination::Notebook(id) => assert_eq!(id.as_str(), dest),
            JexFolderDestination::Stack(id) => assert_eq!(id.as_str(), dest),
        }
        assert_eq!(mapped.raw_sha256, format!("{:x}", Sha256::digest(raw)));
    }
    let root_leaf_stack: Option<String> = db
        .query_row(
            "SELECT stack_id FROM notebooks WHERE id=?1",
            [&root_leaf_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(root_leaf_stack, None);
    for (source_id, expected_notebook, raw, title, html, text) in [
        (
            NOTE_LEAF,
            root_leaf_id.as_str(),
            leaf_note.as_bytes(),
            "叶中笔记",
            None,
            "正文\n附件.pdf",
        ),
        (
            NOTE_CHILD,
            child_row.0.as_str(),
            child_note.as_bytes(),
            "子中笔记",
            Some("<p>网页正文</p>"),
            "网页正文",
        ),
    ] {
        let mapped = report
            .notes
            .iter()
            .find(|n| n.source_id == source_id)
            .unwrap();
        assert_eq!(
            mapped.source_parent_id,
            if source_id == NOTE_LEAF {
                ROOT_LEAF
            } else {
                CHILD
            }
        );
        let loaded = repo.load_note(&mapped.destination_id).unwrap().unwrap();
        assert_eq!(loaded.notebook_id.as_str(), expected_notebook);
        assert_eq!(loaded.title, title);
        if let Some(expected_html) = html {
            assert_eq!(loaded.body_html, expected_html);
        }
        assert_eq!(loaded.body_text, text);
        assert_eq!(
            (loaded.created_time, loaded.updated_time),
            (1700000003000, 1700000004000)
        );
        let audit: (Vec<u8>, String) = db
            .query_row(
                "SELECT raw_item_bytes,source_path FROM jex_stage_note_audit WHERE source_id=?1",
                [source_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(audit.0, raw);
        assert_eq!(audit.1, format!("{source_id}.md"));
        for table in ["search_unicode", "search_trigram"] {
            let indexed: String = db
                .query_row(
                    &format!("SELECT body FROM {table} WHERE note_id=?1"),
                    [loaded.id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(indexed, text);
        }
    }
    let leaf_mapped = report
        .notes
        .iter()
        .find(|n| n.source_id == NOTE_LEAF)
        .unwrap();
    let leaf_loaded = repo
        .load_note(&leaf_mapped.destination_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        leaf_loaded.resource_ids,
        vec![report.resources[0].destination_id.clone()]
    );
    assert!(
        leaf_loaded
            .body_html
            .contains(report.resources[0].destination_id.as_str())
    );
    let (stored, mut file) = repo
        .open_verified_resource_file(&report.resources[0].destination_id)
        .unwrap()
        .unwrap();
    let mut digest = Sha256::new();
    assert_eq!(
        std::io::copy(&mut file, &mut digest).unwrap(),
        pdf.len() as u64
    );
    assert_eq!(stored.sha256.as_str(), format!("{:x}", Sha256::digest(pdf)));
    assert_eq!(format!("{:x}", digest.finalize()), stored.sha256.as_str());
    drop(repo);
    drop(db);
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
fn ambiguous_folder_graphs_and_missing_note_parent_block_without_leaking_a_profile() {
    let cases = [
        (
            "root notes plus children",
            vec![
                folder(ROOT_STACK, "项目", ""),
                folder(CHILD, "合同", ROOT_STACK),
            ],
            ROOT_STACK,
            true,
        ),
        (
            "missing folder parent",
            vec![folder(CHILD, "合同", ROOT_STACK)],
            CHILD,
            true,
        ),
        (
            "folder cycle",
            vec![
                folder(ROOT_STACK, "项目", CHILD),
                folder(CHILD, "合同", ROOT_STACK),
            ],
            CHILD,
            true,
        ),
        (
            "three levels",
            vec![
                folder(ROOT_STACK, "项目", ""),
                folder(CHILD, "合同", ROOT_STACK),
                folder(ROOT_LEAF, "第三层", CHILD),
            ],
            ROOT_LEAF,
            true,
        ),
        (
            "duplicate root title",
            vec![
                folder(ROOT_STACK, "同名", ""),
                folder(ROOT_LEAF, "同名", ""),
            ],
            ROOT_LEAF,
            true,
        ),
        (
            "missing note parent",
            vec![folder(ROOT_LEAF, "根叶", "")],
            ROOT_STACK,
            false,
        ),
        (
            "invalid folder source time",
            vec![
                folder(ROOT_LEAF, "根叶", "")
                    .replace("2022-12-31T23:59:59.000Z", "2022-02-29T23:59:59.000Z"),
            ],
            ROOT_LEAF,
            true,
        ),
        (
            "unexpected folder body",
            vec![folder(ROOT_LEAF, "根叶", "").replacen("\n\nid:", "\n\n不应丢弃的正文\n\nid:", 1)],
            ROOT_LEAF,
            true,
        ),
    ];
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    for (label, folders, note_parent, folder_error) in cases {
        let source = archive(|tar| {
            append(
                tar,
                &format!("{NOTE_LEAF}.md"),
                note(NOTE_LEAF, "笔记", "正文", 1, note_parent).as_bytes(),
            );
            for raw in &folders {
                let id = raw
                    .split("id: ")
                    .nth(1)
                    .unwrap()
                    .split('\n')
                    .next()
                    .unwrap();
                append(tar, &format!("{id}.md"), raw.as_bytes());
            }
        });
        let error = stage_jex_file(&source, parent.path()).unwrap_err();
        match error {
            JexStageError::UnsupportedFolder {
                source_id,
                source_path,
                reason,
            } if folder_error => {
                assert_eq!(source_path, format!("{source_id}.md"), "{label}");
                assert!(!reason.is_empty(), "{label}");
            }
            JexStageError::UnsupportedNote {
                source_id,
                source_path,
                reason,
            } if !folder_error => {
                assert_eq!(source_id, NOTE_LEAF, "{label}");
                assert_eq!(source_path, format!("{NOTE_LEAF}.md"), "{label}");
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
fn blocked_body_after_folder_creation_cleans_owned_child_only() {
    let source = archive(|tar| {
        append(
            tar,
            &format!("{ROOT_LEAF}.md"),
            folder(ROOT_LEAF, "根叶", "").as_bytes(),
        );
        append(
            tar,
            &format!("{NOTE_LEAF}.md"),
            note(NOTE_LEAF, "危险", "<script>alert(1)</script>", 2, ROOT_LEAF).as_bytes(),
        );
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

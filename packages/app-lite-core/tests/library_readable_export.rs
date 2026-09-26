//! Whole-library readable export (HTML/JSON/original resources) and restore
//! into a new, empty library. Distinct from the SQLite snapshot backup.

use std::{fs, path::Path};

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRepository, NoteId, SaveNote, SearchQuery,
    export_library_readable, restore_library_readable,
};
use rusqlite::Connection;
use tempfile::tempdir;

fn text(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

fn with_image(prefix: &str, resource: app_lite_core::ResourceId) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![
            Inline::Text {
                text: prefix.into(),
                marks: Default::default(),
            },
            Inline::Image {
                resource_id: resource,
                alt: "图".into(),
                display_width: None,
                link: None,
            },
        ],
    }])
}

// Same shape as tests/library_backup.rs `library()`, plus an attachment and
// tag order, so both whole-library paths are
// exercised against comparable content.
fn library(root: &Path) -> (std::path::PathBuf, Vec<NoteId>) {
    let profile = root.join("library");
    fs::create_dir(&profile).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let stack = repo.create_stack("项目组").unwrap();
    let inner = repo.create_notebook("合同", Some(&stack.id)).unwrap();
    let outer = repo.create_notebook("日记", None).unwrap();
    let urgent = repo.create_tag("要务").unwrap();
    let later = repo.create_tag("稍后").unwrap();
    let shared_a = repo
        .import_resource(b"shared bytes", "a.png", "image/png", "png")
        .unwrap();
    let shared_b = repo
        .import_resource(b"shared bytes", "b.png", "image/png", "png")
        .unwrap();
    let history_only = repo
        .import_resource(b"old picture", "old.png", "image/png", "png")
        .unwrap();
    let pdf = repo
        .import_resource(b"%PDF-1.7 evidence", "证据.pdf", "application/pdf", "pdf")
        .unwrap();

    let first = repo
        .create_note(CreateNote {
            title: "合同草稿".into(),
            notebook_id: Some(inner.id.clone()),
            document: with_image("甲方乙方", shared_a.clone()),
        })
        .unwrap();
    repo.set_note_tags(&first.id, &[later.id.clone(), urgent.id.clone()])
        .unwrap();
    let second = repo
        .create_note(CreateNote {
            title: "日记".into(),
            notebook_id: Some(outer.id.clone()),
            document: with_image("晴", shared_b.clone()),
        })
        .unwrap();
    let revised = repo
        .create_note(CreateNote {
            title: "改过的".into(),
            notebook_id: None,
            document: with_image("旧版", history_only.clone()),
        })
        .unwrap();
    repo.save_note(SaveNote {
        id: revised.id.clone(),
        expected_revision: revised.revision,
        title: "改过的<b>".into(),
        document: CanonicalDocument::from_blocks(vec![
            text("新版只有附件").blocks()[0].clone(),
            Block::Attachment {
                resource_id: pdf.clone(),
                filename: "证据.pdf".into(),
                media_type: "application/pdf".into(),
            },
        ]),
        resource_ids: vec![pdf],
        selected_thumbnail_id: None,
    })
    .unwrap();
    let trashed = repo
        .create_note(CreateNote {
            title: "已删除".into(),
            notebook_id: None,
            document: text("回收站里的内容"),
        })
        .unwrap();
    repo.trash_note(&trashed.id).unwrap();
    repo.process_search_jobs().unwrap();
    (profile, vec![first.id, second.id, revised.id, trashed.id])
}

/// Logical rows of every table the export claims to carry.
fn rows(profile: &Path) -> Vec<(String, Vec<String>)> {
    let db = Connection::open(profile.join("library.sqlite")).unwrap();
    [
        ("stacks", "SELECT id,title,revision,created_time,updated_time,deleted_time FROM stacks ORDER BY id"),
        ("notebooks", "SELECT id,title,IFNULL(stack_id,''),is_default,revision,created_time,updated_time,deleted_time FROM notebooks ORDER BY id"),
        ("tags", "SELECT id,title,revision,created_time,updated_time,deleted_time FROM tags ORDER BY id"),
        ("note_tags", "SELECT note_id,tag_id,position FROM note_tags ORDER BY note_id,tag_id"),
        ("notes", "SELECT id,title,body_html,body_text,notebook_id,IFNULL(selected_thumbnail_id,''),created_time,updated_time,deleted_time,revision FROM notes ORDER BY id"),
        ("note_resources", "SELECT note_id,position,resource_id,is_associated FROM note_resources ORDER BY note_id,position"),
        ("note_revisions", "SELECT note_id,revision,title,body_html,body_text,created_time FROM note_revisions ORDER BY note_id,revision"),
        ("resources", "SELECT id,sha256,title,mime,file_extension,size,created_time,updated_time,deleted_time,revision FROM resources ORDER BY id"),
        ("resource_blobs", "SELECT sha256 FROM resource_blobs ORDER BY sha256"),
        ("shortcuts", "SELECT id,entity_type,entity_id,position,created_time FROM shortcuts ORDER BY id"),
    ]
    .into_iter()
    .map(|(table, sql)| {
        let mut statement = db.prepare(sql).unwrap();
        let columns = statement.column_count();
        let values = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| {
                        row.get::<_, rusqlite::types::Value>(index)
                            .map(|value| format!("{value:?}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|values| values.join("|"))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        (table.to_owned(), values)
    })
    .collect()
}

#[test]
fn whole_library_readable_export_restores_every_relation_body_history_and_blob() {
    let root = tempdir().unwrap();
    let (profile, notes) = library(root.path());
    // No product path writes these yet, but the format carries them: a
    // soft-deleted resource (fires the filename-search trigger) and a shortcut.
    {
        let db = Connection::open(profile.join("library.sqlite")).unwrap();
        db.execute(
            "UPDATE resources SET deleted_time=7 WHERE title='old.png'",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO shortcuts(id,entity_type,entity_id,position,created_time) VALUES('dddddddddddddddddddddddddddddddd','note',?1,0,5)",
            [notes[0].as_str()],
        )
        .unwrap();
    }
    let source = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let bundle = root.path().join("export");
    let report = export_library_readable(&source, &bundle).unwrap();
    assert_eq!(report.note_count, 4);
    let source_resources: i64 = Connection::open(profile.join("library.sqlite"))
        .unwrap()
        .query_row("SELECT count(*) FROM resources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(report.resource_count as i64, source_resources);

    // Readable without this app: an index grouped by stack/notebook, one page
    // per note, JSON manifest and original resource bytes.
    let index = fs::read_to_string(bundle.join("index.html")).unwrap();
    for expected in ["项目组", "合同", "日记", "回收站", "改过的&lt;b&gt;"] {
        assert!(index.contains(expected), "{expected} missing from index");
    }
    assert!(bundle.join("manifest.json").is_file());
    let page =
        fs::read_to_string(bundle.join(format!("readable/{}.html", notes[2].as_str()))).unwrap();
    assert!(
        page.contains("../resources/"),
        "attachment link is relative"
    );
    let resources: Vec<_> = fs::read_dir(bundle.join("resources")).unwrap().collect();
    assert_eq!(resources.len() as i64, source_resources);

    let restored = root.path().join("restored");
    fs::create_dir(&restored).unwrap();
    let restore = restore_library_readable(&bundle, &restored).unwrap();
    assert_eq!(restore.note_count, 4);
    assert_eq!(rows(&restored), rows(&profile));

    let repo = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    assert_eq!(repo.outbox_count().unwrap(), 0);
    assert_eq!(
        repo.search(SearchQuery::parse("甲方乙方")).unwrap().len(),
        1
    );
}

#[test]
fn whole_library_readable_export_and_restore_fail_closed() {
    let root = tempdir().unwrap();
    let (profile, notes) = library(root.path());
    let source = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let bundle = root.path().join("export");
    export_library_readable(&source, &bundle).unwrap();
    assert!(
        export_library_readable(&source, &bundle).is_err(),
        "target exists"
    );

    let not_empty = root.path().join("not-empty");
    fs::create_dir(&not_empty).unwrap();
    fs::write(not_empty.join("x"), b"x").unwrap();
    assert!(restore_library_readable(&bundle, &not_empty).is_err());

    let tamper = |name: &str, edit: &dyn Fn(&Path)| {
        let copy = root.path().join(format!("tampered-{name}"));
        copy_dir(&bundle, &copy);
        edit(&copy);
        let target = root.path().join(format!("target-{name}"));
        fs::create_dir(&target).unwrap();
        assert!(
            restore_library_readable(&copy, &target).is_err(),
            "{name} must be rejected"
        );
        assert_eq!(
            fs::read_dir(&target).unwrap().count(),
            0,
            "{name} left files"
        );
    };
    tamper("resource", &|copy| {
        let entry = fs::read_dir(copy.join("resources"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        fs::write(entry.path(), b"changed").unwrap();
    });
    tamper("note", &|copy| {
        let path = copy.join(format!("notes/{}.html", notes[0].as_str()));
        let html = fs::read_to_string(&path).unwrap().replace("甲方", "丙方");
        fs::write(path, html).unwrap();
    });
    tamper("history", &|copy| {
        let path = copy.join(format!("history/{}.json", notes[2].as_str()));
        let json = fs::read_to_string(&path).unwrap().replace("旧版", "伪造");
        fs::write(path, json).unwrap();
    });
    tamper("version", &|copy| {
        let path = copy.join("manifest.json");
        let json =
            fs::read_to_string(&path)
                .unwrap()
                .replacen("\"version\":1", "\"version\":99", 1);
        fs::write(path, json).unwrap();
    });
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

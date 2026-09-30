//! Two isolated clients and one server (docs/research/sync-client-design-v1.md
//! §2–3): download, convergence, conflict copies, delete versus edit.

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRepository, ListQuery, NoteId, SaveNote, sync,
};
use app_lite_protocol::{Action, EntityKind, EntityRef, Operation, PROTOCOL_VERSION, PushRequest};
use app_lite_server::ServerStore;
use tempfile::{TempDir, tempdir};

fn text(value: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: value.into(),
            marks: Default::default(),
        }],
    }])
}

struct Client {
    _root: TempDir,
    repo: LibraryRepository,
}

fn client() -> Client {
    let root = tempdir().unwrap();
    let repo = LibraryRepository::open(root.path().join("library.sqlite")).unwrap();
    Client { _root: root, repo }
}

fn server() -> (TempDir, ServerStore) {
    let root = tempdir().unwrap();
    let store = ServerStore::open(root.path()).unwrap();
    (root, store)
}

fn sync(client: &Client, store: &ServerStore) -> sync::SyncReport {
    sync::sync_once(&client.repo, store).unwrap()
}

fn titles(client: &Client) -> Vec<String> {
    let mut titles: Vec<String> = client
        .repo
        .list_notes(ListQuery::default())
        .unwrap()
        .into_iter()
        .map(|note| client.repo.load_note(&note.id).unwrap().unwrap().title)
        .collect();
    titles.sort();
    titles
}

fn save(client: &Client, id: &NoteId, title: &str, body: &str) {
    let note = client.repo.load_note(id).unwrap().unwrap();
    client
        .repo
        .save_note(SaveNote {
            id: id.clone(),
            expected_revision: note.revision,
            title: title.into(),
            document: text(body),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
}

#[test]
fn a_new_device_receives_notes_organization_and_adopts_the_default_notebook() {
    let (_server_root, store) = server();
    let a = client();
    let stack = a.repo.create_stack("工作").unwrap();
    let notebook = a.repo.create_notebook("合同", Some(&stack.id)).unwrap();
    let tag = a.repo.create_tag("要务").unwrap();
    let in_notebook = a
        .repo
        .create_note(CreateNote {
            title: "合同草稿".into(),
            notebook_id: Some(notebook.id.clone()),
            document: text("甲方乙方"),
        })
        .unwrap();
    a.repo
        .set_note_tags(&in_notebook.id, &[tag.id.clone()])
        .unwrap();
    let in_default = a
        .repo
        .create_note(CreateNote {
            title: "随手记".into(),
            notebook_id: None,
            document: text("默认笔记本里"),
        })
        .unwrap();
    sync(&a, &store);

    let b = client();
    sync(&b, &store);
    let received = b
        .repo
        .load_note(&in_notebook.id)
        .unwrap()
        .expect("note arrives");
    assert_eq!(received.title, "合同草稿");
    assert_eq!(received.body_html, in_notebook.body_html);
    assert_eq!(received.notebook_id, notebook.id);
    assert_eq!(received.tag_ids, vec![tag.id.clone()]);
    let index = b.repo.list_navigation_index().unwrap();
    assert!(format!("{index:?}").contains("工作"), "stack arrives");
    // B's empty default notebook became A's, so notes stay in "the" default.
    assert_eq!(
        b.repo.default_notebook().unwrap().id,
        a.repo.default_notebook().unwrap().id
    );
    assert_eq!(
        b.repo
            .load_note(&in_default.id)
            .unwrap()
            .unwrap()
            .notebook_id,
        a.repo.default_notebook().unwrap().id
    );
    // Receiving does not echo back as local changes.
    assert_eq!(b.repo.outbox_count().unwrap(), 0);
    let report = sync(&b, &store);
    assert_eq!(
        report.accepted, 0,
        "nothing to upload after a pure download"
    );
}

#[test]
fn unrelated_offline_edits_on_two_devices_converge() {
    let (_server_root, store) = server();
    let a = client();
    let shared = a
        .repo
        .create_note(CreateNote {
            title: "共享".into(),
            notebook_id: None,
            document: text("一"),
        })
        .unwrap();
    sync(&a, &store);
    let b = client();
    sync(&b, &store);
    save(&a, &shared.id, "共享（A改）", "A 的修改");
    b.repo
        .create_note(CreateNote {
            title: "B 新建".into(),
            notebook_id: None,
            document: text("B"),
        })
        .unwrap();
    sync(&a, &store);
    sync(&b, &store);
    sync(&a, &store);
    assert_eq!(titles(&a), ["B 新建", "共享（A改）"]);
    assert_eq!(titles(&a), titles(&b));
}

#[test]
fn concurrent_edits_of_one_note_keep_both_as_a_visible_conflict_copy() {
    let (_server_root, store) = server();
    let a = client();
    let note = a
        .repo
        .create_note(CreateNote {
            title: "会议".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    sync(&a, &store);
    let b = client();
    sync(&b, &store);
    save(&a, &note.id, "会议", "A 写的结论");
    save(&b, &note.id, "会议", "B 写的结论");
    sync(&a, &store);
    let report = sync(&b, &store);
    assert!(report.conflicts > 0);
    sync(&a, &store);
    for device in [&a, &b] {
        let original = device.repo.load_note(&note.id).unwrap().unwrap();
        assert!(
            original.body_text.contains("A 写的结论"),
            "server version wins the id"
        );
        let notes = device.repo.list_notes(ListQuery::default()).unwrap();
        let copy = notes
            .iter()
            .map(|n| device.repo.load_note(&n.id).unwrap().unwrap())
            .find(|n| n.id != note.id && n.title.contains("冲突副本"))
            .expect("conflict copy is visible on both devices");
        assert!(copy.body_text.contains("B 写的结论"), "no edit is lost");
    }
    // The device that made the copy lists it until the person settles it.
    let open = b.repo.sync_conflicts().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].original_id, note.id.as_str());
    assert_eq!(open[0].copy_title, "会议（冲突副本）");
    assert_eq!(open[0].original_title.as_deref(), Some("会议"));
    assert!(b.repo.sync_resolve_conflict(&open[0].copy_id).unwrap());
    assert!(b.repo.sync_conflicts().unwrap().is_empty());
    assert!(
        b.repo.load_note(&open[0].copy_id).unwrap().is_some(),
        "settling keeps the copy"
    );
    assert!(!b.repo.sync_resolve_conflict(&open[0].copy_id).unwrap());
}

#[test]
fn deleting_on_one_device_never_silently_drops_an_edit_from_the_other() {
    let (_server_root, store) = server();
    let a = client();
    let note = a
        .repo
        .create_note(CreateNote {
            title: "待删".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    sync(&a, &store);
    let b = client();
    sync(&b, &store);
    a.repo.trash_note(&note.id).unwrap();
    a.repo.purge_note(&note.id).unwrap();
    save(&b, &note.id, "待删", "B 在删除前写下的重要内容");
    sync(&a, &store);
    sync(&b, &store);
    sync(&a, &store);
    for device in [&a, &b] {
        let kept = device
            .repo
            .list_notes(ListQuery::default())
            .unwrap()
            .into_iter()
            .map(|n| device.repo.load_note(&n.id).unwrap().unwrap())
            .any(|n| n.body_text.contains("B 在删除前写下的重要内容"));
        assert!(kept, "the edit survives the remote delete");
    }
}

#[test]
fn a_malformed_remote_body_is_a_visible_failure_and_does_not_block_later_changes() {
    let (_server_root, store) = server();
    let a = client();
    sync(&a, &store);
    // Another client (or a damaged server) publishes a non-canonical body.
    let bad = "b".repeat(32);
    let device = "e".repeat(32);
    store
        .push(PushRequest {
            protocol: PROTOCOL_VERSION,
            device_id: device.clone(),
            ops: vec![Operation {
                op_id: "f".repeat(32),
                device_id: device,
                entity: EntityRef {
                    kind: EntityKind::Note,
                    id: bad.clone(),
                },
                base_revision: 0,
                action: Action::Put {
                    payload: serde_json::json!({
                        "title": "坏的",
                        "body_html": "<script>alert(1)</script>",
                        "notebook_id": a.repo.default_notebook().unwrap().id.as_str(),
                        "tag_ids": [], "resource_ids": [],
                        "created_time": 1, "updated_time": 1, "deleted_time": 0,
                    }),
                },
            }],
        })
        .unwrap();
    let a2 = a
        .repo
        .create_note(CreateNote {
            title: "之后的".into(),
            notebook_id: None,
            document: text("x"),
        })
        .unwrap();
    sync(&a, &store);
    let b = client();
    sync(&b, &store);
    assert!(
        b.repo
            .load_note(&NoteId::parse(&bad).unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        b.repo.load_note(&a2.id).unwrap().is_some(),
        "later changes still apply"
    );
    let failures = sync::sync_failures(&b.repo).unwrap();
    let skipped = failures
        .iter()
        .find(|failure| failure.entity_id == bad)
        .expect("the skipped change is visible");
    assert!(!skipped.can_retry, "a download cannot be retried from here");
    assert!(!sync::retry_failure(&b.repo, &skipped.op_id).unwrap());
    assert_eq!(sync::sync_failures(&b.repo).unwrap(), failures, "kept");
}

#[test]
fn legacy_note_payload_works_but_invalid_cover_references_are_rejected() {
    let (_root, store) = server();
    let a = client();
    let resource = a.repo.import_resource(b"file", "test.txt", "text/plain", "txt").unwrap();
    let document = CanonicalDocument::from_blocks(vec![Block::Attachment {
        resource_id: resource.clone(), filename: "test.txt".into(), media_type: "text/plain".into(),
    }]);
    a.repo.create_note(CreateNote { title: "seed".into(), notebook_id: None, document: document.clone() }).unwrap();
    sync(&a, &store);
    let variants = [None, Some(serde_json::json!(7)), Some(serde_json::json!("../invalid")),
        Some(serde_json::json!("a".repeat(32))), Some(serde_json::json!(resource.as_str()))];
    let device = "e".repeat(32);
    for (index, cover) in variants.into_iter().enumerate() {
        let mut payload = serde_json::json!({
            "title": "legacy", "body_html": document.to_canonical_html().as_str(),
            "notebook_id": a.repo.default_notebook().unwrap().id.as_str(),
            "tag_ids": [], "resource_ids": [resource.as_str()],
            "created_time": 1, "updated_time": 1, "deleted_time": 0,
        });
        if let Some(cover) = cover { payload["selected_thumbnail_id"] = cover; }
        store.push(PushRequest {
            protocol: PROTOCOL_VERSION, device_id: device.clone(), ops: vec![Operation {
                op_id: format!("{:032x}", index + 100), device_id: device.clone(),
                entity: EntityRef { kind: EntityKind::Note, id: format!("{:032x}", index + 1) },
                base_revision: 0, action: Action::Put { payload },
            }],
        }).unwrap();
    }
    let b = client();
    let report = sync(&b, &store);
    assert_eq!(report.skipped, 4);
    assert!(b.repo.load_note(&NoteId::parse(format!("{:032x}", 1)).unwrap()).unwrap().is_some());
    for index in 2..=5 {
        assert!(b.repo.load_note(&NoteId::parse(format!("{index:032x}")).unwrap()).unwrap().is_none());
    }
    assert_eq!(b.repo.sync_failures().unwrap().len(), 4);
}

#[test]
fn identical_offline_edits_do_not_create_a_conflict_copy() {
    let (_server_root, store) = server();
    let a = client();
    let note = a
        .repo
        .create_note(CreateNote {
            title: "同改".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    sync(&a, &store);
    let b = client();
    sync(&b, &store);
    save(&a, &note.id, "同改", "两边写了一样的字");
    save(&b, &note.id, "同改", "两边写了一样的字");
    sync(&a, &store);
    sync(&b, &store);
    sync(&a, &store);
    for device in [&a, &b] {
        assert_eq!(titles(device), ["同改"], "no copy when the content agrees");
    }
}

#[test]
fn a_library_restored_from_backup_is_a_new_device_that_keeps_unsynced_edits() {
    let (_server_root, store) = server();
    let a = client();
    let note = a
        .repo
        .create_note(CreateNote {
            title: "备份前".into(),
            notebook_id: None,
            document: text("已同步"),
        })
        .unwrap();
    sync(&a, &store);
    // An edit made after the last sync is in the backup but not on the server.
    save(&a, &note.id, "备份前", "只在备份里的修改");
    let work = tempdir().unwrap();
    let backup = work.path().join("backup");
    app_lite_core::backup_library(
        a._root.path(),
        &backup,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let restored_dir = work.path().join("restored");
    app_lite_core::restore_library_backup(
        &backup,
        &restored_dir,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let restored = LibraryRepository::open(restored_dir.join("library.sqlite")).unwrap();
    assert_ne!(
        restored.sync_device_id().unwrap(),
        a.repo.sync_device_id().unwrap(),
        "a restored library never reuses the old device identity"
    );
    sync::sync_once(&restored, &store).unwrap();
    let b = client();
    sync(&b, &store);
    let kept = b
        .repo
        .list_notes(ListQuery::default())
        .unwrap()
        .into_iter()
        .map(|n| b.repo.load_note(&n.id).unwrap().unwrap())
        .any(|n| n.body_text.contains("只在备份里的修改"));
    assert!(
        kept,
        "the unsynced edit reaches the server from the restored library"
    );
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The server is restored from an older backup. A client whose cursor is
/// now ahead of the server must notice, re-upload what the server lost and
/// still receive changes made after the restore.
#[test]
fn a_server_restored_from_an_older_backup_loses_nothing() {
    let server_root = tempdir().unwrap();
    let backup = tempdir().unwrap();
    let a = client();
    let store = ServerStore::open(server_root.path()).unwrap();
    let first = a
        .repo
        .create_note(CreateNote {
            title: "备份前".into(),
            notebook_id: None,
            document: text("一"),
        })
        .unwrap();
    sync(&a, &store);
    drop(store);
    copy_dir(server_root.path(), &backup.path().join("server"));
    let store = ServerStore::open(server_root.path()).unwrap();
    let lost = a
        .repo
        .create_note(CreateNote {
            title: "备份后才上传".into(),
            notebook_id: None,
            document: text("二"),
        })
        .unwrap();
    sync(&a, &store);
    drop(store);

    // Restore the older backup over the server data.
    std::fs::remove_dir_all(server_root.path()).unwrap();
    copy_dir(&backup.path().join("server"), server_root.path());
    let store = ServerStore::open(server_root.path()).unwrap();
    let b = client();
    sync(&b, &store);
    let later = b
        .repo
        .create_note(CreateNote {
            title: "恢复之后新建".into(),
            notebook_id: None,
            document: text("三"),
        })
        .unwrap();
    sync(&b, &store);

    sync(&a, &store);
    sync(&b, &store);
    for device in [&a, &b] {
        for id in [&first.id, &lost.id, &later.id] {
            assert!(device.repo.load_note(id).unwrap().is_some(), "{id:?}");
        }
        assert_eq!(titles(device).len(), 3, "no spurious conflict copies");
    }
}

#[test]
fn an_unsaved_edit_overtaken_by_a_remote_version_is_kept_as_a_conflict_copy() {
    let (_server_root, store) = server();
    let a = client();
    let note = a
        .repo
        .create_note(CreateNote {
            title: "同一篇".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    let tag = a.repo.create_tag("标签").unwrap();
    a.repo.set_note_tags(&note.id, &[tag.id.clone()]).unwrap();
    sync(&a, &store);
    let b = client();
    sync(&b, &store);
    save(&b, &note.id, "同一篇", "另一台设备改的");
    sync(&b, &store);
    let opened = a.repo.load_note(&note.id).unwrap().unwrap();
    sync(&a, &store);
    // The editor still holds an edit based on `opened`, now overtaken.
    let copy = a
        .repo
        .save_overtaken_edit_as_conflict_copy(
            &note.id,
            opened.revision,
            "同一篇",
            &text("本机未保存"),
        )
        .unwrap();
    assert_eq!(copy.title, "同一篇（冲突副本）");
    assert!(copy.body_html.contains("本机未保存"));
    assert_eq!(copy.notebook_id, opened.notebook_id);
    assert_eq!(copy.tag_ids, vec![tag.id.clone()]);
    assert!(
        a.repo
            .load_note(&note.id)
            .unwrap()
            .unwrap()
            .body_html
            .contains("另一台设备改的")
    );
    sync(&a, &store);
    sync(&b, &store);
    assert!(
        b.repo.load_note(&copy.id).unwrap().is_some(),
        "the copy syncs"
    );
    assert_eq!(a.repo.sync_conflicts().unwrap().len(), 1);
    a.repo.trash_note(&copy.id).unwrap();
    assert!(
        a.repo.sync_conflicts().unwrap().is_empty(),
        "a trashed copy is settled"
    );
}

/// Evernote moves a deleted notebook's notes to Trash (main-readable 32150
/// localization `ModalManager.deleteNotebook.confirmation`). Another device
/// must see the same Trash, and a restore there must not bring the notebook
/// back or leave a note pointing at it.
#[test]
fn a_notebook_deleted_on_one_device_leaves_its_notes_in_trash_on_both() {
    let (_server_root, store) = server();
    let a = client();
    let notebook = a.repo.create_notebook("旧本", None).unwrap();
    let image = a
        .repo
        .import_image(b"synced notebook image", "a.png", "image/png", "png")
        .unwrap();
    let with_image = a
        .repo
        .create_note(CreateNote {
            title: "带图".into(),
            notebook_id: Some(notebook.id.clone()),
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Image {
                    resource_id: image.clone(),
                    alt: "图".into(),
                    display_width: None,
                    link: None,
                }],
            }]),
        })
        .unwrap();
    let edited = a
        .repo
        .create_note(CreateNote {
            title: "改过".into(),
            notebook_id: Some(notebook.id.clone()),
            document: text("第一版"),
        })
        .unwrap();
    save(&a, &edited.id, "改过", "第二版");
    let earlier = a
        .repo
        .create_note(CreateNote {
            title: "早已删除".into(),
            notebook_id: Some(notebook.id.clone()),
            document: text("早就在废纸篓"),
        })
        .unwrap();
    a.repo.trash_note(&earlier.id).unwrap();
    sync(&a, &store);
    let b = client();
    sync(&b, &store);

    let history = |client: &Client, id: &NoteId| -> i64 {
        rusqlite::Connection::open(client._root.path().join("library.sqlite"))
            .unwrap()
            .query_row(
                "SELECT count(*) FROM note_revisions WHERE note_id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    };
    let history_before = history(&a, &edited.id);
    a.repo.delete_notebook(&notebook.id).unwrap();
    sync(&a, &store);
    sync(&b, &store);

    for id in [&with_image.id, &edited.id, &earlier.id] {
        let note = b.repo.load_note(id).unwrap().expect("still on B");
        assert!(
            note.deleted_time.is_some(),
            "{} is in Trash on B",
            note.title
        );
    }
    assert_eq!(
        b.repo.load_note(&edited.id).unwrap().unwrap().body_html,
        a.repo.load_note(&edited.id).unwrap().unwrap().body_html
    );
    assert_eq!(
        b.repo.read_resource_bytes(&image).unwrap().unwrap(),
        b"synced notebook image"
    );
    assert_eq!(
        history(&a, &edited.id),
        history_before,
        "A keeps the history"
    );
    assert!(
        b.repo
            .list_navigation_index()
            .unwrap()
            .notebooks
            .iter()
            .all(|candidate| candidate.id != notebook.id)
    );

    b.repo.restore_note(&with_image.id).unwrap();
    let default = b.repo.default_notebook().unwrap().id;
    assert_eq!(
        b.repo
            .load_note(&with_image.id)
            .unwrap()
            .unwrap()
            .notebook_id,
        default
    );
    sync(&b, &store);
    sync(&a, &store);

    let back = a.repo.load_note(&with_image.id).unwrap().unwrap();
    assert!(back.deleted_time.is_none(), "the restore reaches A");
    assert_eq!(back.notebook_id, a.repo.default_notebook().unwrap().id);
    assert_eq!(
        a.repo.read_resource_bytes(&image).unwrap().unwrap(),
        b"synced notebook image"
    );
    for client in [&a, &b] {
        assert!(
            client
                .repo
                .load_note(&edited.id)
                .unwrap()
                .unwrap()
                .deleted_time
                .is_some()
        );
        let index = client.repo.list_navigation_index().unwrap();
        assert!(
            index
                .notebooks
                .iter()
                .all(|candidate| candidate.id != notebook.id),
            "not resurrected"
        );
        for note in client.repo.list_notes(ListQuery::default()).unwrap() {
            assert!(
                index
                    .notebooks
                    .iter()
                    .any(|candidate| candidate.id == note.notebook_id),
                "no live note points at a missing notebook"
            );
        }
    }
}

/// Superscript and subscript, with an underline inside the superscript as
/// Evernote nests it, survive A -> server -> B -> edit -> A -> reopen.
#[test]
fn superscript_and_subscript_survive_a_round_trip_through_another_device() {
    round_trip_through_another_device(
        "<p>E=mc<sup><u>2</u></sup> and H<sub>2</sub>O</p>",
        "<p>E=mc<sup><u>2</u></sup> and H<sub>2</sub>O, edited on B</p>",
    );
}

/// Text colour, with its alpha and dark-mode inversion marker, survives the
/// same round trip.
#[test]
fn text_colour_survives_a_round_trip_through_another_device() {
    round_trip_through_another_device(
        "<p><span style=\"color: #fc1233\">红</span><span style=\"color: rgba(24, 133, 226, 0.502); --inversion-type-color: simple\">蓝</span></p>",
        "<p><span style=\"color: #fc1233\">红</span><span style=\"color: rgba(24, 133, 226, 0.502); --inversion-type-color: simple\">蓝</span>, edited on B</p>",
    );
}

fn round_trip_through_another_device(html: &str, edited: &str) {
    let (_server_root, store) = server();
    let a = client();
    let note = a
        .repo
        .create_note(CreateNote {
            title: "公式".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html(html).unwrap(),
        })
        .unwrap();
    assert_eq!(note.body_html, html);
    sync(&a, &store);

    let b = client();
    sync(&b, &store);
    let received = b.repo.load_note(&note.id).unwrap().unwrap();
    assert_eq!(received.body_html, html);
    b.repo
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: received.revision,
            title: received.title,
            document: CanonicalDocument::parse_html(edited).unwrap(),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    sync(&b, &store);
    sync(&a, &store);

    let path = a._root.path().join("library.sqlite");
    let root = a._root;
    drop(a.repo);
    let reopened = LibraryRepository::open(path).unwrap();
    assert_eq!(
        reopened.load_note(&note.id).unwrap().unwrap().body_html,
        edited
    );
    drop(root);
}

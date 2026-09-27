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
    assert!(
        sync::sync_failures(&b.repo)
            .unwrap()
            .iter()
            .any(|failure| failure.entity_id == bad),
        "the skipped change is visible"
    );
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

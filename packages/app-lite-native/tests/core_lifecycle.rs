use joplin_lite_native::core::{CreateNote, NoteRepository, UpdateNote};
use rusqlite::Connection;
use tempfile::tempdir;

#[test]
fn note_lifecycle_survives_restart_and_supports_fts() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("notes.sqlite");
    let repo = NoteRepository::open(&path).unwrap();

    let created = repo
        .create_note(CreateNote {
            title: "原生笔记".into(),
            body: "离线正文与搜索".into(),
            body_rtf: Vec::new(),
            is_draft: false,
        })
        .unwrap();
    let id = created.id.clone();
    assert_eq!(repo.search("搜索").unwrap()[0].id, id);
    repo.update_note(
        &id,
        UpdateNote {
            title: Some("更新后的标题".into()),
            body: Some("持久化后的正文".into()),
            body_rtf: None,
        },
    )
    .unwrap();
    drop(repo);

    let reopened = NoteRepository::open(&path).unwrap();
    let note = reopened.get_note(&id).unwrap().unwrap();
    assert_eq!(note.title, "更新后的标题");
    assert_eq!(note.body, "持久化后的正文");
    reopened.soft_delete(&id).unwrap();
    assert!(reopened.get_note(&id).unwrap().is_none());
}

#[test]
fn abandoned_blank_drafts_are_removed_but_nonempty_drafts_remain() {
    let dir = tempdir().unwrap();
    let repo = NoteRepository::open(dir.path().join("notes.sqlite")).unwrap();
    let blank = repo
        .create_note(CreateNote {
            title: "".into(),
            body: "".into(),
            body_rtf: Vec::new(),
            is_draft: true,
        })
        .unwrap();
    let nonempty = repo
        .create_note(CreateNote {
            title: "".into(),
            body: "用户已经输入".into(),
            body_rtf: Vec::new(),
            is_draft: true,
        })
        .unwrap();

    assert_eq!(repo.cleanup_abandoned_drafts().unwrap(), 1);
    assert!(repo.get_note(&blank.id).unwrap().is_none());
    assert!(repo.get_note(&nonempty.id).unwrap().is_some());
}

#[test]
fn search_index_failure_does_not_block_note_writes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("notes.sqlite");
    let repo = NoteRepository::open(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE notes_fts")
        .unwrap();

    let created = repo
        .create_note(CreateNote {
            title: "仍可保存".into(),
            body: "索引损坏不应阻塞正文".into(),
            body_rtf: Vec::new(),
            is_draft: false,
        })
        .unwrap();
    assert_eq!(
        repo.get_note(&created.id).unwrap().unwrap().title,
        "仍可保存"
    );
}

#[test]
fn soft_delete_commits_when_search_index_is_missing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("notes.sqlite");
    let repo = NoteRepository::open(&path).unwrap();
    let created = repo
        .create_note(CreateNote {
            title: "待删除".into(),
            body: "正文".into(),
            body_rtf: Vec::new(),
            is_draft: false,
        })
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE notes_fts")
        .unwrap();

    repo.soft_delete(&created.id).unwrap();
    assert!(repo.get_note(&created.id).unwrap().is_none());
}

#[test]
fn rich_text_payload_survives_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("notes.sqlite");
    let repo = NoteRepository::open(&path).unwrap();
    let expected_rtf = b"{\\rtf1\\b native}".to_vec();
    let created = repo
        .create_note(CreateNote {
            title: "格式化".into(),
            body: "native".into(),
            body_rtf: expected_rtf.clone(),
            is_draft: false,
        })
        .unwrap();
    drop(repo);

    let reopened = NoteRepository::open(&path).unwrap();
    assert_eq!(
        reopened.get_note(&created.id).unwrap().unwrap().body_rtf,
        expected_rtf
    );
}

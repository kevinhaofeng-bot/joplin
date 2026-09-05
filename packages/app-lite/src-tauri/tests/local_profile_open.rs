#![cfg(target_os = "macos")]

use std::{path::PathBuf, time::Duration};

use joplin_lite::profile::ProfilePaths;
use joplin_lite::sync_sidecar::{
    CreateFolderParams, CreateNoteParams, CreateTagParams, ExpectedUpdatedTimeParams,
    GetByIdParams, ListNotesParams, MarkupLanguage, ProfileState, SetNoteTagsParams, SidecarClient,
    SidecarCommand, SidecarErrorKind, SidecarState, UpdateFolderParams, UpdateNoteParams,
};

fn command(repo_root: PathBuf) -> SidecarCommand {
    SidecarCommand {
        executable: PathBuf::from("node"),
        args: vec![
            "-r".into(),
            "./packages/app-lite-sync/node_modules/ts-node/register/transpile-only".into(),
            "packages/app-lite-sync/src/main.ts".into(),
        ],
        current_dir: repo_root,
    }
}

#[tokio::test]
async fn opens_closes_and_reopens_one_isolated_profile() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let parent = std::env::temp_dir().join(format!(
        "joplin-lite-local-open-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let root = parent.join("com.kevinhao.joplin-lite");
    std::fs::create_dir_all(&parent).expect("temporary parent");
    let paths = ProfilePaths::try_from_app_data(root.clone()).expect("profile paths");
    paths.ensure().expect("profile scaffold");

    let result = async {
        let mut first = SidecarClient::start_for_profile(
            command(repo_root.clone()),
            &paths,
            Duration::from_secs(10),
        )
        .await
        .expect("supervised profile sidecar");
        assert_eq!(first.state(), SidecarState::Ready);
        assert_eq!(
            first.profile_status().await.expect("closed status").state,
            ProfileState::Closed
        );
        assert_eq!(
            first.open_profile().await.expect("open profile").state,
            ProfileState::Open
        );
        assert!(root.join(".joplin-lite-profile.json").is_file());
        assert!(paths.database().is_file());

        let folder_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        let child_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned();
        let created_folder = first
            .create_folder(CreateFolderParams {
                id: Some(folder_id.clone()),
                parent_id: "".into(),
                title: "/Root".into(),
            })
            .await
            .expect("create root folder");
        assert_eq!(created_folder.item.title, "Root");
        assert!(
            !first
                .create_folder(CreateFolderParams {
                    id: Some(folder_id.clone()),
                    parent_id: "".into(),
                    title: "Root".into()
                })
                .await
                .expect("replay root folder")
                .created
        );
        first
            .create_folder(CreateFolderParams {
                id: Some(child_id.clone()),
                parent_id: folder_id.clone(),
                title: "Child".into(),
            })
            .await
            .expect("create child folder");
        let root_updated_time = created_folder.item.updated_time;
        let child_conflict = first
            .update_folder(UpdateFolderParams {
                id: child_id.clone(),
                expected_updated_time: 0,
                title: Some("Child 2".into()),
                parent_id: None,
            })
            .await;
        assert_eq!(
            child_conflict.unwrap_err().kind(),
            SidecarErrorKind::Conflict
        );
        assert_eq!(first.list_folders().await.expect("list folders").len(), 2);
        let cycle = first
            .update_folder(UpdateFolderParams {
                id: folder_id.clone(),
                expected_updated_time: root_updated_time,
                title: None,
                parent_id: Some(child_id.clone()),
            })
            .await;
        assert_eq!(
            cycle.unwrap_err().kind(),
            SidecarErrorKind::ValidationFailed
        );

        let tag_id = "cccccccccccccccccccccccccccccccc".to_owned();
        let second_tag_id = "dddddddddddddddddddddddddddddddd".to_owned();
        let tag = first
            .create_tag(CreateTagParams {
                id: Some(tag_id.clone()),
                title: "  Cafe\u{0301}  ".into(),
            })
            .await
            .expect("create tag");
        assert_eq!(tag.item.title, "Café");
        assert_eq!(tag.item.note_count, 0);
        first
            .create_tag(CreateTagParams {
                id: Some(second_tag_id.clone()),
                title: "Second".into(),
            })
            .await
            .expect("create second tag");

        let note_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_owned();
        let created_note = first
            .create_note(CreateNoteParams {
                id: Some(note_id.clone()),
                parent_id: child_id.clone(),
                title: "Local note".into(),
                body: "Hello from the local profile".into(),
                is_todo: None,
                todo_due: None,
            })
            .await
            .expect("create note");
        assert_eq!(created_note.item.body, "Hello from the local profile");
        assert_eq!(created_note.item.markup_language, MarkupLanguage::Markdown);
        assert!(
            !first
                .create_note(CreateNoteParams {
                    id: Some(note_id.clone()),
                    parent_id: child_id.clone(),
                    title: "Local note".into(),
                    body: "Hello from the local profile".into(),
                    is_todo: None,
                    todo_due: None,
                })
                .await
                .expect("replay note")
                .created
        );
        let listed_notes = first
            .list_notes(ListNotesParams {
                parent_id: Some(child_id.clone()),
                page: None,
                limit: None,
            })
            .await
            .expect("list notes");
        assert_eq!(listed_notes.items.len(), 1);
        let detail = first
            .get_note(GetByIdParams {
                id: note_id.clone(),
            })
            .await
            .expect("get note");
        assert_eq!(detail.body, "Hello from the local profile");

        let tagged = first
            .set_note_tags(SetNoteTagsParams {
                note_id: note_id.clone(),
                expected_updated_time: created_note.item.updated_time,
                tag_ids: vec![second_tag_id.clone(), tag_id.clone()],
            })
            .await
            .expect("set note tags");
        assert_eq!(tagged.tag_ids, vec![tag_id.clone(), second_tag_id.clone()]);
        let replaced = first
            .set_note_tags(SetNoteTagsParams {
                note_id: note_id.clone(),
                expected_updated_time: tagged.updated_time,
                tag_ids: vec![second_tag_id],
            })
            .await
            .expect("replace note tags");
        let updated_note = first
            .update_note(UpdateNoteParams {
                id: note_id.clone(),
                expected_updated_time: replaced.updated_time,
                title: None,
                body: Some("Updated local profile note".into()),
                parent_id: None,
                is_todo: None,
                todo_due: None,
                todo_completed: None,
            })
            .await
            .expect("update note");
        assert_eq!(updated_note.item.body, "Updated local profile note");
        let stale_note = first
            .update_note(UpdateNoteParams {
                id: note_id.clone(),
                expected_updated_time: replaced.updated_time,
                title: Some("stale".into()),
                body: None,
                parent_id: None,
                is_todo: None,
                todo_due: None,
                todo_completed: None,
            })
            .await;
        assert_eq!(stale_note.unwrap_err().kind(), SidecarErrorKind::Conflict);
        first.shutdown().await.expect("first shutdown");
        assert_eq!(first.state(), SidecarState::Stopped);

        let mut second =
            SidecarClient::start_for_profile(command(repo_root), &paths, Duration::from_secs(10))
                .await
                .expect("reopened profile sidecar");
        assert_eq!(
            second.open_profile().await.expect("reopen profile").state,
            ProfileState::Open
        );
        assert_eq!(
            second.list_folders().await.expect("reopened folders").len(),
            2
        );
        assert_eq!(second.list_tags().await.expect("reopened tags").len(), 2);
        let reopened_detail = second
            .get_note(GetByIdParams {
                id: note_id.clone(),
            })
            .await
            .expect("reopened note");
        assert_eq!(reopened_detail.body, "Updated local profile note");
        assert_eq!(
            reopened_detail.tag_ids,
            vec!["dddddddddddddddddddddddddddddddd"]
        );
        assert_eq!(
            second
                .list_notes(ListNotesParams {
                    parent_id: Some(child_id.clone()),
                    page: None,
                    limit: None
                })
                .await
                .expect("reopened notes")
                .items
                .len(),
            1
        );
        second
            .trash_note(ExpectedUpdatedTimeParams {
                id: note_id.clone(),
                expected_updated_time: reopened_detail.updated_time,
            })
            .await
            .expect("trash reopened note");
        assert!(
            second
                .list_notes(ListNotesParams {
                    parent_id: Some(child_id),
                    page: None,
                    limit: None
                })
                .await
                .expect("notes after trash")
                .items
                .is_empty()
        );
        second
            .trash_folder(ExpectedUpdatedTimeParams {
                id: folder_id,
                expected_updated_time: root_updated_time,
            })
            .await
            .expect("trash folder");
        assert!(
            second
                .list_folders()
                .await
                .expect("folders after trash")
                .is_empty()
        );
        second.shutdown().await.expect("second shutdown");
    };

    result.await;
    std::fs::remove_dir_all(parent).expect("temporary profile cleanup");
}

#![cfg(target_os = "macos")]

use std::{path::PathBuf, time::Duration};

use joplin_lite::profile::ProfilePaths;
use joplin_lite::sync_sidecar::{SidecarClient, SidecarCommand, SidecarErrorKind, SidecarState};
use serde_json::json;

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
            first
                .request("profileStatus", json!({}))
                .await
                .expect("closed status")["state"],
            "closed"
        );
        let opened = first
            .request("openProfile", json!({ "profilePath": root }))
            .await
            .expect("open profile");
        assert_eq!(opened["state"], "open");
        assert!(opened["schemaVersion"].as_u64().unwrap_or(0) > 0);
        assert!(root.join(".joplin-lite-profile.json").is_file());
        assert!(paths.database().is_file());
        assert!(
            std::fs::metadata(paths.database())
                .expect("database metadata")
                .len()
                > 0
        );

        let folder_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let child_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let created_folder = first
            .request(
                "createFolder",
                json!({ "id": folder_id, "parentId": "", "title": "/Root" }),
            )
            .await
            .expect("create root folder");
        assert_eq!(created_folder["item"]["title"], "Root");
        assert_eq!(
            first
                .request(
                    "createFolder",
                    json!({ "id": folder_id, "parentId": "", "title": "Root" }),
                )
                .await
                .expect("replay root folder")["created"],
            false
        );
        first
            .request(
                "createFolder",
                json!({ "id": child_id, "parentId": folder_id, "title": "Child" }),
            )
            .await
            .expect("create child folder");
        let root_updated_time = created_folder["item"]["updatedTime"]
            .as_u64()
            .expect("root timestamp");
        let child = first
            .request(
                "updateFolder",
                json!({ "id": child_id, "expectedUpdatedTime": 0, "title": "Child 2" }),
            )
            .await;
        assert_eq!(child.unwrap_err().kind(), SidecarErrorKind::Conflict);
        let child = first
            .request("listFolders", json!({}))
            .await
            .expect("list folders");
        assert_eq!(child.as_array().expect("folder list").len(), 2);
        let cycle = first
            .request(
                "updateFolder",
                json!({ "id": folder_id, "expectedUpdatedTime": root_updated_time, "parentId": child_id }),
            )
            .await;
        assert_eq!(
            cycle.unwrap_err().kind(),
            SidecarErrorKind::ValidationFailed
        );
        let tag_id = "cccccccccccccccccccccccccccccccc";
        let second_tag_id = "dddddddddddddddddddddddddddddddd";
        let tag = first
            .request(
                "createTag",
                json!({ "id": tag_id, "title": "  Cafe\u{0301}  " }),
            )
            .await
            .expect("create tag");
        assert_eq!(tag["item"]["title"], "Café");
        assert_eq!(tag["item"]["noteCount"], 0);
        first
            .request(
                "createTag",
                json!({ "id": second_tag_id, "title": "Second" }),
            )
            .await
            .expect("create second tag");

        let note_id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let created_note = first
            .request(
                "createNote",
                json!({ "id": note_id, "parentId": child_id, "title": "Local note", "body": "Hello from the local profile" }),
            )
            .await
            .expect("create note");
        assert_eq!(created_note["item"]["body"], "Hello from the local profile");
        assert_eq!(
            first
                .request(
                    "createNote",
                    json!({ "id": note_id, "parentId": child_id, "title": "Local note", "body": "Hello from the local profile" }),
                )
                .await
                .expect("replay note")["created"],
            false
        );
        let listed_notes = first
            .request("listNotes", json!({ "parentId": child_id }))
            .await
            .expect("list notes");
        assert!(listed_notes["items"][0].get("body").is_none());
        assert_eq!(
            first
                .request("getNote", json!({ "id": note_id }))
                .await
                .expect("get note")["body"],
            "Hello from the local profile"
        );
        let note_updated_time = created_note["item"]["updatedTime"]
            .as_u64()
            .expect("note timestamp");
        let tagged = first
            .request(
                "setNoteTags",
                json!({ "noteId": note_id, "expectedUpdatedTime": note_updated_time, "tagIds": [second_tag_id, tag_id] }),
            )
            .await
            .expect("set note tags");
        assert_eq!(tagged["tagIds"], json!([tag_id, second_tag_id]));
        let replaced = first
            .request(
                "setNoteTags",
                json!({ "noteId": note_id, "expectedUpdatedTime": tagged["updatedTime"], "tagIds": [second_tag_id] }),
            )
            .await
            .expect("replace note tags");
        assert_eq!(replaced["tagIds"], json!([second_tag_id]));
        let updated_note = first
            .request(
                "updateNote",
                json!({ "id": note_id, "expectedUpdatedTime": replaced["updatedTime"], "body": "Updated local profile note" }),
            )
            .await
            .expect("update note");
        let stale_note = first
            .request(
                "updateNote",
                json!({ "id": note_id, "expectedUpdatedTime": replaced["updatedTime"], "title": "stale" }),
            )
            .await;
        assert_eq!(stale_note.unwrap_err().kind(), SidecarErrorKind::Conflict);
        first
            .request(
                "trashNote",
                json!({ "id": note_id, "expectedUpdatedTime": updated_note["item"]["updatedTime"] }),
            )
            .await
            .expect("trash note");
        assert!(
            first
                .request("listNotes", json!({ "parentId": child_id }))
                .await
                .expect("notes after trash")["items"]
                .as_array()
                .expect("note list")
                .is_empty()
        );

        first
            .request(
                "trashFolder",
                json!({ "id": folder_id, "expectedUpdatedTime": root_updated_time }),
            )
            .await
            .expect("trash folder");
        assert_eq!(
            first
                .request("listFolders", json!({}))
                .await
                .expect("folders after trash")
                .as_array()
                .expect("folder list")
                .len(),
            0
        );

        first.shutdown().await.expect("first shutdown");
        assert_eq!(first.state(), SidecarState::Stopped);

        let mut second =
            SidecarClient::start_for_profile(command(repo_root), &paths, Duration::from_secs(10))
                .await
                .expect("reopened profile sidecar");
        let reopened = second
            .request("openProfile", json!({ "profilePath": root }))
            .await
            .expect("reopen profile");
        assert_eq!(reopened["state"], "open");
        assert!(
            second
                .request("listNotes", json!({}))
                .await
                .expect("reopened notes")["items"]
                .as_array()
                .expect("note list")
                .is_empty()
        );
        assert_eq!(
            second
                .request("getNote", json!({ "id": note_id }))
                .await
                .unwrap_err()
                .kind(),
            SidecarErrorKind::NotFound
        );
        second.shutdown().await.expect("second shutdown");
    };

    result.await;
    std::fs::remove_dir_all(parent).expect("temporary profile cleanup");
}

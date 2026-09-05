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

        let tag_id = "cccccccccccccccccccccccccccccccc";
        let tag = first
            .request(
                "createTag",
                json!({ "id": tag_id, "title": "  Cafe\u{0301}  " }),
            )
            .await
            .expect("create tag");
        assert_eq!(tag["item"]["title"], "Café");
        assert_eq!(tag["item"]["noteCount"], 0);
        let tag_updated_time = tag["item"]["updatedTime"].as_u64().expect("tag timestamp");
        let updated_tag = first
            .request(
                "updateTag",
                json!({ "id": tag_id, "expectedUpdatedTime": tag_updated_time, "title": "Updated" }),
            )
            .await
            .expect("update tag");
        let updated_tag_time = updated_tag["item"]["updatedTime"]
            .as_u64()
            .expect("updated tag timestamp");
        first
            .request(
                "deleteTag",
                json!({ "id": tag_id, "expectedUpdatedTime": updated_tag_time }),
            )
            .await
            .expect("delete tag");
        assert!(
            first
                .request("listTags", json!({}))
                .await
                .expect("tags after delete")
                .as_array()
                .expect("tag list")
                .is_empty()
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
                .request("listFolders", json!({}))
                .await
                .expect("reopened folders")
                .as_array()
                .expect("folder list")
                .is_empty()
        );
        second.shutdown().await.expect("second shutdown");
    };

    result.await;
    std::fs::remove_dir_all(parent).expect("temporary profile cleanup");
}

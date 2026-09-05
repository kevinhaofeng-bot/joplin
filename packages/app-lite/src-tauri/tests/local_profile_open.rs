#![cfg(target_os = "macos")]

use std::{path::PathBuf, time::Duration};

use joplin_lite::profile::ProfilePaths;
use joplin_lite::sync_sidecar::{SidecarClient, SidecarCommand, SidecarState};
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
        second.shutdown().await.expect("second shutdown");
    };

    result.await;
    std::fs::remove_dir_all(parent).expect("temporary profile cleanup");
}

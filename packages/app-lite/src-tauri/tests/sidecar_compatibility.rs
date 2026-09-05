use std::{path::PathBuf, time::Duration};

use joplin_lite::sync_sidecar::{SidecarClient, SidecarCommand, SidecarState};
use serde_json::json;

#[tokio::test]
async fn official_node_sidecar_round_trips_note_fixture() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let note_path = repo_root.join("packages/app-lite-sync/fixtures/v1/note.txt");
    let note_raw = std::fs::read_to_string(note_path).expect("note fixture");
    let command = SidecarCommand {
        executable: PathBuf::from("corepack"),
        args: vec![
            "yarn".into(),
            "workspace".into(),
            "@joplin/app-lite-sync".into(),
            "start:stdio".into(),
        ],
        current_dir: repo_root,
    };

    let mut client = SidecarClient::start(command, Duration::from_secs(10))
        .await
        .expect("real sidecar starts");
    let decoded = client
        .request("decodeItem", json!({ "raw": note_raw }))
        .await
        .expect("decode fixture");
    assert_eq!(decoded["id"], "11111111111111111111111111111111");

    let encoded = client
        .request("encodeItem", json!({ "item": decoded }))
        .await
        .expect("encode fixture");
    let decoded_again = client
        .request("decodeItem", json!({ "raw": encoded }))
        .await
        .expect("decode encoded fixture");
    assert_eq!(decoded_again["title"], "Fixture note");
    assert!(
        decoded_again["body"]
            .as_str()
            .unwrap()
            .contains("第二行中文")
    );

    let shutdown_started = std::time::Instant::now();
    client.shutdown().await.expect("shutdown");
    assert!(
        shutdown_started.elapsed() < Duration::from_secs(2),
        "sidecar shutdown consumed the grace period: {:?}",
        shutdown_started.elapsed()
    );
    assert_eq!(client.state(), SidecarState::Stopped);
}

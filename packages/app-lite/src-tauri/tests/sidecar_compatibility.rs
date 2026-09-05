use std::{path::PathBuf, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout, Command},
};

async fn request(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: &mut u32,
    command: &str,
    params: Value,
) -> Value {
    let request_id = format!("request-{}", *id);
    *id += 1;
    let frame =
        json!({ "id": request_id, "protocolVersion": 1, "command": command, "params": params });
    stdin
        .write_all(format!("{}\n", serde_json::to_string(&frame).expect("request")).as_bytes())
        .await
        .expect("write request");
    stdin.flush().await.expect("flush request");
    let mut line = String::new();
    stdout.read_line(&mut line).await.expect("read response");
    let response: Value = serde_json::from_str(&line).expect("response JSON");
    assert_eq!(response["ok"], true, "sidecar response: {response}");
    response["result"].clone()
}

#[tokio::test]
async fn official_node_sidecar_round_trips_note_fixture() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let note_path = repo_root.join("packages/app-lite-sync/fixtures/v1/note.txt");
    let note_raw = std::fs::read_to_string(note_path).expect("note fixture");
    let mut child = Command::new("corepack")
        .args(["yarn", "workspace", "@joplin/app-lite-sync", "start:stdio"])
        .current_dir(repo_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("real sidecar starts");
    let mut stdin = child.stdin.take().expect("sidecar stdin");
    let stdout = child.stdout.take().expect("sidecar stdout");
    let mut stdout = BufReader::new(stdout);
    let mut id = 1;

    let hello = request(&mut stdin, &mut stdout, &mut id, "hello", json!({})).await;
    assert_eq!(hello["protocolVersion"], 1);
    let decoded = request(
        &mut stdin,
        &mut stdout,
        &mut id,
        "decodeItem",
        json!({ "raw": note_raw }),
    )
    .await;
    assert_eq!(decoded["id"], "11111111111111111111111111111111");
    let encoded = request(
        &mut stdin,
        &mut stdout,
        &mut id,
        "encodeItem",
        json!({ "item": decoded }),
    )
    .await;
    let decoded_again = request(
        &mut stdin,
        &mut stdout,
        &mut id,
        "decodeItem",
        json!({ "raw": encoded }),
    )
    .await;
    assert_eq!(decoded_again["title"], "Fixture note");
    assert!(
        decoded_again["body"]
            .as_str()
            .expect("body")
            .contains("第二行中文")
    );
    let shutdown_started = std::time::Instant::now();
    let shutdown = request(&mut stdin, &mut stdout, &mut id, "shutdown", json!({})).await;
    assert_eq!(shutdown, json!({ "stopped": true }));
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .expect("sidecar shutdown timeout")
        .expect("sidecar wait");
    assert!(status.success());
    assert!(shutdown_started.elapsed() < Duration::from_secs(2));
}

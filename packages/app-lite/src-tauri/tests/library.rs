use joplin_lite::library::{LibraryError, LibraryState};
use joplin_lite::sync_sidecar::{SidecarError, SidecarErrorKind};

#[cfg(target_os = "macos")]
use joplin_lite::{profile::ProfilePaths, sync_sidecar::SidecarCommand};

#[test]
fn library_error_serializes_only_stable_code_and_message() {
    let error = LibraryError::from(SidecarError::new(
        SidecarErrorKind::StorageError,
        "secret profile path and SQL payload",
    ));
    let json = serde_json::to_value(error).unwrap();
    assert_eq!(json["code"], "STORAGE_ERROR");
    assert_eq!(json["message"], "无法保存资料库");
    assert!(!json.to_string().contains("secret"));
    assert!(!json.to_string().contains("SQL"));
}

#[tokio::test]
async fn unavailable_library_fails_without_starting_a_sidecar() {
    let state = LibraryState::unavailable();
    let error = state.open().await.unwrap_err();
    assert_eq!(error.code, "SIDECAR_UNAVAILABLE");
    assert_eq!(error.message, "本地资料库不可用");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn first_business_call_lazily_opens_and_open_is_idempotent() {
    let parent = std::env::temp_dir().join(format!(
        "joplin-lite-library-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&parent).unwrap();
    let paths = ProfilePaths::try_from_app_data(parent.join("com.kevinhao.joplin-lite")).unwrap();
    paths.ensure().unwrap();
    let fake = parent.join("fake-sidecar.js");
    std::fs::write(
        &fake,
        r#"const readline=require('readline');let opened=false;
const send=(id,result,error)=>process.stdout.write(JSON.stringify(error?{id,ok:false,error}:{id,ok:true,result})+'\n');
readline.createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);
if(r.command==='hello') send(r.id,{protocolVersion:1});
else if(r.command==='openProfile'){if(opened) send(r.id,null,{code:'PROFILE_ALREADY_OPEN',message:'not for callers'});else {opened=true;send(r.id,{state:'open',schemaVersion:53,formatVersion:1});}}
else if(r.command==='listFolders') send(r.id,[]);
else if(r.command==='shutdown'){send(r.id,{stopped:true});process.exit(0);}
else send(r.id,{});});"#,
    )
    .unwrap();
    let command = SidecarCommand {
        executable: "node".into(),
        args: vec![fake.to_string_lossy().into_owned()],
        current_dir: parent.clone(),
    };
    let state = LibraryState::new(paths, command);

    let folders = state.list_folders().await.unwrap();
    assert!(folders.is_empty());
    let first = state.open().await.unwrap();
    let second = state.open().await.unwrap();
    assert_eq!(first, second);
    let retried = state.retry().await.unwrap();
    assert_eq!(retried, first);
    assert!(state.list_folders().await.unwrap().is_empty());
    state.shutdown().await.unwrap();
    std::fs::remove_dir_all(parent).unwrap();
}

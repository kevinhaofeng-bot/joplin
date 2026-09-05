use std::{fmt, io, process::Stdio, time::Duration};

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{
    process::{Child, Command},
    time::timeout,
};
use tokio_util::codec::{Decoder, Encoder, FramedRead, FramedWrite};

use crate::profile::ProfilePaths;

use super::domain::{
    ConfigureJoplinServerParams, CreateFolderParams, CreateNoteParams,
    CreateResourceFromPathParams, CreateResult, CreateTagParams, DeleteResult, EmptyParams,
    ExpectedUpdatedTimeParams, Folder, GetByIdParams, ListNoteResourcesParams, ListNotesParams,
    NoteDetail, NotePage, OpenProfile, ProfilePathParams, ProfileStatus, Resource, SearchNotePage,
    SearchNotesParams, SetNoteTagsParams, SetNoteTagsResult, ShutdownResult, SyncConfig,
    SyncStatus, SyncSummary, Tag, TrashResult, UpdateFolderParams, UpdateNoteParams, UpdateResult,
};
use super::profile_lease::ProfileLease;
use super::protocol::{
    MAX_FRAME_BYTES, ResponseFrame, STARTUP_TIMEOUT, SidecarCommand, SidecarError,
    SidecarErrorKind, SidecarState, classify_response_error, decode_response,
    is_recoverable_error_code, request_frame, validate_hello, validate_response_id,
};

type SidecarReader = FramedRead<tokio::process::ChildStdout, RawNdjsonCodec>;
type SidecarWriter = FramedWrite<tokio::process::ChildStdin, RawNdjsonCodec>;

#[derive(Debug)]
enum FrameError {
    TooLarge,
    Truncated,
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("frame too large"),
            Self::Truncated => formatter.write_str("truncated frame"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for FrameError {}
impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct RawNdjsonCodec;

impl Decoder for RawNdjsonCodec {
    type Item = Vec<u8>;
    type Error = FrameError;

    fn decode(&mut self, source: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if let Some(index) = source.iter().position(|byte| *byte == b'\n') {
            let frame_length = index + 1;
            if frame_length > MAX_FRAME_BYTES {
                return Err(FrameError::TooLarge);
            }
            let frame = source.split_to(frame_length);
            let mut payload = frame[..index].to_vec();
            if payload.last() == Some(&b'\r') {
                payload.pop();
            }
            return Ok(Some(payload));
        }
        if source.len() >= MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        Ok(None)
    }

    fn decode_eof(&mut self, source: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if source.is_empty() {
            Ok(None)
        } else {
            Err(FrameError::Truncated)
        }
    }
}

impl Encoder<String> for RawNdjsonCodec {
    type Error = FrameError;
    fn encode(&mut self, frame: String, destination: &mut BytesMut) -> Result<(), Self::Error> {
        if frame.len() + 1 > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        destination.reserve(frame.len() + 1);
        destination.extend_from_slice(frame.as_bytes());
        destination.extend_from_slice(b"\n");
        Ok(())
    }
}

#[derive(Debug)]
pub struct SidecarClient {
    child: Child,
    lease: Option<ProfileLease>,
    reader: SidecarReader,
    writer: SidecarWriter,
    state: SidecarState,
    request_timeout: Duration,
    next_request: u64,
    profile_root: Option<std::path::PathBuf>,
}

impl SidecarClient {
    pub async fn start(
        command: SidecarCommand,
        request_timeout: Duration,
    ) -> Result<Self, SidecarError> {
        Self::start_with_lease(command, request_timeout, None, None).await
    }

    #[cfg(target_os = "macos")]
    pub async fn start_for_profile(
        command: SidecarCommand,
        profile: &ProfilePaths,
        request_timeout: Duration,
    ) -> Result<Self, SidecarError> {
        profile
            .ensure()
            .map_err(|error| SidecarError::new(SidecarErrorKind::ProfileLockRequired, error))?;
        let lease = ProfileLease::acquire(profile)?;
        Self::start_with_lease(
            command,
            request_timeout,
            Some(lease),
            Some(profile.root().to_path_buf()),
        )
        .await
    }

    async fn start_with_lease(
        command: SidecarCommand,
        request_timeout: Duration,
        mut lease: Option<ProfileLease>,
        profile_root: Option<std::path::PathBuf>,
    ) -> Result<Self, SidecarError> {
        let mut process = Command::new(&command.executable);
        process
            .args(&command.args)
            .current_dir(&command.current_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(profile_lease) = lease.as_ref() {
            let source_fd = profile_lease.raw_fd();
            process.env("JOPLIN_LITE_PROFILE_LEASE_FD", "198");
            unsafe {
                process.pre_exec(move || {
                    if libc::dup2(source_fd, 198) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(198, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        } else {
            process.env_remove("JOPLIN_LITE_PROFILE_LEASE_FD");
        }
        let mut child = process
            .spawn()
            .map_err(|error| SidecarError::new(SidecarErrorKind::SpawnFailed, error))?;
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(SidecarError::new(
                    SidecarErrorKind::Io,
                    "sidecar stdin unavailable",
                ));
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(SidecarError::new(
                    SidecarErrorKind::Io,
                    "sidecar stdout unavailable",
                ));
            }
        };
        let mut client = Self {
            child,
            lease: lease.take(),
            reader: FramedRead::new(stdout, RawNdjsonCodec),
            writer: FramedWrite::new(stdin, RawNdjsonCodec),
            state: SidecarState::Starting,
            request_timeout,
            next_request: 2,
            profile_root,
        };

        let handshake = timeout(
            STARTUP_TIMEOUT,
            client.exchange("request-1", "hello", json!({})),
        )
        .await;
        let response = match handshake {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                client.fail(error.kind()).await;
                return Err(error);
            }
            Err(_) => {
                let error = SidecarError::new(SidecarErrorKind::Timeout, "startup timeout");
                client.fail(error.kind()).await;
                return Err(error);
            }
        };
        if let Err(error) = validate_response_id(&response, "request-1") {
            client.fail(error.kind()).await;
            return Err(error);
        }
        if !response.ok {
            let error = classify_response_error(&response);
            client.fail(error.kind()).await;
            return Err(error);
        }
        if let Err(error) = validate_hello(&response) {
            client.fail(error.kind()).await;
            return Err(error);
        }
        client.state = SidecarState::Ready;
        Ok(client)
    }

    async fn request_value_with_timeout(
        &mut self,
        command: &str,
        params: Value,
        request_timeout: Duration,
    ) -> Result<Value, SidecarError> {
        if self.state != SidecarState::Ready {
            return Err(SidecarError::new(
                SidecarErrorKind::InvalidResponse,
                "sidecar is not ready",
            ));
        }
        let id = format!("request-{}", self.next_request);
        self.next_request += 1;
        let exchange = timeout(request_timeout, self.exchange(&id, command, params)).await;
        let response = match exchange {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                self.fail(error.kind()).await;
                return Err(error);
            }
            Err(_) => {
                let error = SidecarError::new(SidecarErrorKind::Timeout, "request timeout");
                self.fail(error.kind()).await;
                return Err(error);
            }
        };
        if let Err(error) = validate_response_id(&response, &id) {
            self.fail(error.kind()).await;
            return Err(error);
        }
        if !response.ok {
            let error = classify_response_error(&response);
            if !is_recoverable_error_code(
                response
                    .error
                    .as_ref()
                    .map(|error| error.code.as_str())
                    .unwrap_or_default(),
            ) || matches!(
                error.kind(),
                SidecarErrorKind::ProfileLockRequired
                    | SidecarErrorKind::ProfileOpenFailed
                    | SidecarErrorKind::StorageError
            ) {
                self.fail(error.kind()).await;
            }
            return Err(error);
        }
        match response.result {
            Some(result) => Ok(result),
            None => {
                let error = SidecarError::new(SidecarErrorKind::InvalidResponse, "missing result");
                self.fail(error.kind()).await;
                Err(error)
            }
        }
    }

    async fn request_value(&mut self, command: &str, params: Value) -> Result<Value, SidecarError> {
        self.request_value_with_timeout(command, params, self.request_timeout)
            .await
    }

    async fn request_typed<P, R>(&mut self, command: &str, params: &P) -> Result<R, SidecarError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let params = serde_json::to_value(params)
            .map_err(|_| SidecarError::new(SidecarErrorKind::Io, "serialize params"))?;
        let result = self.request_value(command, params).await?;
        match serde_json::from_value(result) {
            Ok(value) => Ok(value),
            Err(_) => {
                let error = SidecarError::new(SidecarErrorKind::InvalidResponse, "invalid result");
                self.fail(error.kind()).await;
                Err(error)
            }
        }
    }

    async fn request_typed_with_timeout<P, R>(
        &mut self,
        command: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<R, SidecarError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let params = serde_json::to_value(params)
            .map_err(|_| SidecarError::new(SidecarErrorKind::Io, "serialize params"))?;
        let result = self
            .request_value_with_timeout(command, params, timeout)
            .await?;
        match serde_json::from_value(result) {
            Ok(value) => Ok(value),
            Err(_) => {
                let error = SidecarError::new(SidecarErrorKind::InvalidResponse, "invalid result");
                self.fail(error.kind()).await;
                Err(error)
            }
        }
    }

    pub async fn profile_status(&mut self) -> Result<ProfileStatus, SidecarError> {
        self.request_typed("profileStatus", &EmptyParams {}).await
    }

    pub async fn open_profile(&mut self) -> Result<OpenProfile, SidecarError> {
        let profile_path = match self.profile_root.clone() {
            Some(path) => path,
            None => {
                let error = SidecarError::new(
                    SidecarErrorKind::ProfileLockRequired,
                    "profile root unavailable",
                );
                self.fail(error.kind()).await;
                return Err(error);
            }
        };
        self.request_typed(
            "openProfile",
            &ProfilePathParams {
                profile_path: profile_path.to_string_lossy().into_owned(),
            },
        )
        .await
    }

    pub async fn list_folders(&mut self) -> Result<Vec<Folder>, SidecarError> {
        self.request_typed("listFolders", &EmptyParams {}).await
    }

    pub async fn create_folder(
        &mut self,
        params: CreateFolderParams,
    ) -> Result<CreateResult<Folder>, SidecarError> {
        self.request_typed("createFolder", &params).await
    }

    pub async fn update_folder(
        &mut self,
        params: UpdateFolderParams,
    ) -> Result<UpdateResult<Folder>, SidecarError> {
        self.request_typed("updateFolder", &params).await
    }

    pub async fn trash_folder(
        &mut self,
        params: ExpectedUpdatedTimeParams,
    ) -> Result<TrashResult, SidecarError> {
        self.request_typed("trashFolder", &params).await
    }

    pub async fn list_tags(&mut self) -> Result<Vec<Tag>, SidecarError> {
        self.request_typed("listTags", &EmptyParams {}).await
    }

    pub async fn create_tag(
        &mut self,
        params: CreateTagParams,
    ) -> Result<CreateResult<Tag>, SidecarError> {
        self.request_typed("createTag", &params).await
    }

    pub async fn update_tag(
        &mut self,
        params: super::domain::UpdateTagParams,
    ) -> Result<UpdateResult<Tag>, SidecarError> {
        self.request_typed("updateTag", &params).await
    }

    pub async fn delete_tag(
        &mut self,
        params: ExpectedUpdatedTimeParams,
    ) -> Result<DeleteResult, SidecarError> {
        self.request_typed("deleteTag", &params).await
    }

    pub async fn list_notes(&mut self, params: ListNotesParams) -> Result<NotePage, SidecarError> {
        self.request_typed("listNotes", &params).await
    }

    pub async fn get_note(&mut self, params: GetByIdParams) -> Result<NoteDetail, SidecarError> {
        self.request_typed("getNote", &params).await
    }

    pub async fn create_note(
        &mut self,
        params: CreateNoteParams,
    ) -> Result<CreateResult<NoteDetail>, SidecarError> {
        self.request_typed("createNote", &params).await
    }

    pub async fn update_note(
        &mut self,
        params: UpdateNoteParams,
    ) -> Result<UpdateResult<NoteDetail>, SidecarError> {
        self.request_typed("updateNote", &params).await
    }

    pub async fn trash_note(
        &mut self,
        params: ExpectedUpdatedTimeParams,
    ) -> Result<TrashResult, SidecarError> {
        self.request_typed("trashNote", &params).await
    }

    pub async fn set_note_tags(
        &mut self,
        params: SetNoteTagsParams,
    ) -> Result<SetNoteTagsResult, SidecarError> {
        self.request_typed("setNoteTags", &params).await
    }

    pub async fn create_resource_from_path(
        &mut self,
        params: CreateResourceFromPathParams,
    ) -> Result<Resource, SidecarError> {
        self.request_typed("createResourceFromPath", &params).await
    }

    pub async fn list_note_resources(
        &mut self,
        params: ListNoteResourcesParams,
    ) -> Result<Vec<Resource>, SidecarError> {
        self.request_typed("listNoteResources", &params).await
    }

    pub async fn get_sync_config(&mut self) -> Result<SyncConfig, SidecarError> {
        self.request_typed("getSyncConfig", &EmptyParams {}).await
    }

    pub async fn configure_joplin_server(
        &mut self,
        params: ConfigureJoplinServerParams,
    ) -> Result<SyncConfig, SidecarError> {
        self.request_typed("configureJoplinServer", &params).await
    }

    pub async fn start_sync(&mut self) -> Result<SyncStatus, SidecarError> {
        self.request_typed("startSync", &EmptyParams {}).await
    }

    pub async fn get_sync_status(&mut self) -> Result<SyncStatus, SidecarError> {
        self.request_typed("getSyncStatus", &EmptyParams {}).await
    }

    pub async fn sync_now(&mut self) -> Result<SyncSummary, SidecarError> {
        self.request_typed_with_timeout("syncNow", &EmptyParams {}, Duration::from_secs(15 * 60))
            .await
    }

    pub async fn search_notes(
        &mut self,
        params: SearchNotesParams,
    ) -> Result<SearchNotePage, SidecarError> {
        self.request_typed("searchNotes", &params).await
    }

    pub fn state(&self) -> SidecarState {
        self.state
    }

    pub async fn shutdown(&mut self) -> Result<(), SidecarError> {
        self.shutdown_with_budget(Duration::from_secs(5)).await
    }

    async fn shutdown_with_budget(&mut self, budget: Duration) -> Result<(), SidecarError> {
        if self.state == SidecarState::Stopped {
            return Ok(());
        }
        if self.state == SidecarState::Failed {
            self.terminate().await;
            self.state = SidecarState::Stopped;
            return Ok(());
        }
        self.state = SidecarState::Stopping;
        let id = format!("request-{}", self.next_request);
        self.next_request += 1;
        let deadline = tokio::time::Instant::now() + budget;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let shutdown_result = timeout(remaining, self.exchange(&id, "shutdown", json!({}))).await;
        let result = match shutdown_result {
            Ok(Ok(response)) => {
                if validate_response_id(&response, &id).is_err() || !response.ok {
                    Err(SidecarError::new(
                        SidecarErrorKind::InvalidResponse,
                        "shutdown response",
                    ))
                } else if response
                    .result
                    .and_then(|result| serde_json::from_value::<ShutdownResult>(result).ok())
                    .is_some_and(|result| result.stopped)
                {
                    Ok(())
                } else {
                    Err(SidecarError::new(
                        SidecarErrorKind::InvalidResponse,
                        "shutdown result",
                    ))
                }
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(SidecarError::new(
                SidecarErrorKind::Timeout,
                "shutdown timeout",
            )),
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let waited = timeout(remaining, self.child.wait()).await;
        let mut natural_exit = matches!(&waited, Ok(Ok(status)) if status.success());
        if waited.is_err() || self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
            natural_exit = false;
        }
        self.lease.take();
        self.state = SidecarState::Stopped;
        if result.is_ok() && !natural_exit {
            Err(SidecarError::new(
                SidecarErrorKind::InvalidResponse,
                "sidecar exit status",
            ))
        } else {
            result
        }
    }

    async fn exchange(
        &mut self,
        id: &str,
        command: &str,
        params: Value,
    ) -> Result<ResponseFrame, SidecarError> {
        let frame = serde_json::to_string(&request_frame(id.to_owned(), command, params))
            .map_err(|_| SidecarError::new(SidecarErrorKind::Io, "serialize request"))?;
        if frame.len() + 1 > MAX_FRAME_BYTES {
            return Err(SidecarError::new(
                SidecarErrorKind::FrameTooLarge,
                "request too large",
            ));
        }
        if let Err(error) = self.writer.send(frame).await {
            return Err(self.map_frame_error(error));
        }
        match self.reader.next().await {
            Some(Ok(bytes)) => {
                let line = String::from_utf8(bytes).map_err(|_| {
                    SidecarError::new(SidecarErrorKind::InvalidResponse, "invalid utf8")
                })?;
                decode_response(&line)
            }
            Some(Err(error)) => Err(self.map_frame_error(error)),
            None => Err(SidecarError::new(
                SidecarErrorKind::SidecarExited,
                "sidecar eof",
            )),
        }
    }

    async fn fail(&mut self, kind: SidecarErrorKind) {
        self.state = SidecarState::Failed;
        self.terminate().await;
        let _ = kind;
    }

    async fn terminate(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill().await;
        }
        let _ = self.child.wait().await;
        self.lease.take();
    }

    fn map_frame_error(&mut self, error: FrameError) -> SidecarError {
        if matches!(error, FrameError::Io(_)) && self.child.try_wait().ok().flatten().is_some() {
            SidecarError::new(SidecarErrorKind::SidecarExited, "sidecar exited")
        } else {
            map_frame_error(error)
        }
    }
}

fn map_frame_error(error: FrameError) -> SidecarError {
    match error {
        FrameError::TooLarge => {
            SidecarError::new(SidecarErrorKind::FrameTooLarge, "frame too large")
        }
        FrameError::Truncated => {
            SidecarError::new(SidecarErrorKind::InvalidResponse, "truncated frame")
        }
        FrameError::Io(_) => SidecarError::new(SidecarErrorKind::Io, "codec io"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use serde_json::json;

    use super::*;

    static PROFILE_TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn command(script: &str) -> SidecarCommand {
        SidecarCommand {
            executable: PathBuf::from("node"),
            args: vec!["-e".into(), script.into()],
            current_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        }
    }

    const ECHO_SCRIPT: &str = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else if(r.command==='shutdown'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{stopped:true}})+'\n');process.exit(0);}else process.stdout.write(JSON.stringify({id:r.id,ok:true,result:r.params})+'\n');});
"#;

    const DOMAIN_FAILURE_SCRIPT: &str = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else if(r.command==='domain')process.stdout.write(JSON.stringify({id:r.id,ok:false,error:{code:'INVALID_ITEM',message:'secret-domain-detail'}})+'\n');else if(r.command==='shutdown'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{stopped:true}})+'\n');process.exit(0);}else process.stdout.write(JSON.stringify({id:r.id,ok:true,result:r.params})+'\n');});
"#;

    const TYPED_CONFLICT_SCRIPT: &str = r#"
const rl=require('readline').createInterface({input:process.stdin});let failed=false;
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else if(r.command==='listFolders'&&!failed){failed=true;process.stdout.write(JSON.stringify({id:r.id,ok:false,error:{code:'CONFLICT',message:'secret-conflict-detail'}})+'\n');}else if(r.command==='shutdown'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{stopped:true}})+'\n');process.exit(0);}else process.stdout.write(JSON.stringify({id:r.id,ok:true,result:[]})+'\n');});
"#;

    const STORAGE_FAILURE_SCRIPT: &str = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else process.stdout.write(JSON.stringify({id:r.id,ok:false,error:{code:'STORAGE_ERROR',message:'secret-storage-detail'}})+'\n');});
"#;

    const LEASE_SCRIPT: &str = r#"
const fs=require('fs');
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else if(r.command==='check'){let open=false;try{fs.fstatSync(198);open=true;}catch{}process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{env:process.env.JOPLIN_LITE_PROFILE_LEASE_FD||null,open}})+'\n');}else if(r.command==='shutdown'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{stopped:true}})+'\n');process.exit(0);}});
"#;

    const PROTOCOL_MISMATCH_FAILURE_SCRIPT: &str = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else if(r.command==='mismatch')process.stdout.write(JSON.stringify({id:r.id,ok:false,error:{code:'PROTOCOL_MISMATCH',message:'secret-version-detail'}})+'\n');else if(r.command==='shutdown'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{stopped:true}})+'\n');process.exit(0);}else process.stdout.write(JSON.stringify({id:r.id,ok:true,result:r.params})+'\n');});
"#;

    #[tokio::test]
    async fn valid_hello_and_echo_are_correlated() {
        let mut client = SidecarClient::start(command(ECHO_SCRIPT), Duration::from_secs(1))
            .await
            .expect("sidecar starts");
        assert_eq!(client.state(), SidecarState::Ready);
        let result = client
            .request_value("echo", json!({"value":"ok"}))
            .await
            .expect("echo");
        assert_eq!(result["value"], "ok");
        client.shutdown().await.expect("shutdown");
        assert_eq!(client.state(), SidecarState::Stopped);
    }

    #[tokio::test]
    async fn domain_failure_is_redacted_and_keeps_client_ready() {
        let mut client =
            SidecarClient::start(command(DOMAIN_FAILURE_SCRIPT), Duration::from_secs(1))
                .await
                .expect("sidecar starts");
        let error = client
            .request_value("domain", json!({"secret":"secret-marker"}))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::InvalidResponse);
        assert_eq!(error.to_string(), "兼容组件响应无效");
        assert!(!error.to_string().contains("secret-domain-detail"));
        assert_eq!(client.state(), SidecarState::Ready);
        assert_eq!(
            client
                .request_value("echo", json!({"after":true}))
                .await
                .unwrap()["after"],
            true
        );
        client.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn typed_conflict_is_recoverable_and_next_typed_request_succeeds() {
        let mut client =
            SidecarClient::start(command(TYPED_CONFLICT_SCRIPT), Duration::from_secs(1))
                .await
                .expect("sidecar starts");
        let error = client.list_folders().await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::Conflict);
        assert_eq!(client.state(), SidecarState::Ready);
        assert!(
            client
                .list_folders()
                .await
                .expect("next typed request")
                .is_empty()
        );
        client.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn typed_storage_error_fails_and_reaps_client() {
        let mut client =
            SidecarClient::start(command(STORAGE_FAILURE_SCRIPT), Duration::from_secs(1))
                .await
                .expect("sidecar starts");
        let error = client.list_folders().await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::StorageError);
        assert_eq!(client.state(), SidecarState::Failed);
        assert!(client.child.try_wait().expect("wait status").is_some());
    }

    #[tokio::test]
    async fn ordinary_protocol_mismatch_is_redacted_and_fails_client() {
        let mut client = SidecarClient::start(
            command(PROTOCOL_MISMATCH_FAILURE_SCRIPT),
            Duration::from_secs(1),
        )
        .await
        .expect("sidecar starts");
        let error = client
            .request_value("mismatch", json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::ProtocolMismatch);
        assert_eq!(error.to_string(), "兼容协议版本不匹配");
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn profile_start_inherits_fd_198_but_ordinary_start_does_not() {
        let mut ordinary = SidecarClient::start(command(LEASE_SCRIPT), Duration::from_secs(1))
            .await
            .expect("ordinary sidecar starts");
        let ordinary_result = ordinary
            .request_value("check", json!({}))
            .await
            .expect("ordinary check");
        assert_eq!(ordinary_result["env"], Value::Null);
        assert_eq!(ordinary_result["open"], false);
        ordinary.shutdown().await.expect("ordinary shutdown");

        let parent = std::env::temp_dir().join(format!(
            "joplin-lite-client-{}-{}",
            std::process::id(),
            PROFILE_TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let root = parent.join(crate::profile::EXPECTED_PROFILE_DIRECTORY_NAME);
        std::fs::create_dir_all(&parent).unwrap();
        let paths = crate::profile::ProfilePaths::try_from_app_data(root).unwrap();
        paths.ensure().unwrap();
        let mut profile =
            SidecarClient::start_for_profile(command(LEASE_SCRIPT), &paths, Duration::from_secs(1))
                .await
                .expect("profile sidecar starts");
        let profile_result = profile
            .request_value("check", json!({}))
            .await
            .expect("profile check");
        assert_eq!(profile_result["env"], "198");
        assert_eq!(profile_result["open"], true);
        profile.shutdown().await.expect("profile shutdown");
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn profile_lease_blocks_second_start_until_first_reaped() {
        let parent = std::env::temp_dir().join(format!(
            "joplin-lite-contention-{}-{}",
            std::process::id(),
            PROFILE_TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let root = parent.join(crate::profile::EXPECTED_PROFILE_DIRECTORY_NAME);
        std::fs::create_dir_all(&parent).unwrap();
        let paths = crate::profile::ProfilePaths::try_from_app_data(root).unwrap();
        paths.ensure().unwrap();
        let mut first =
            SidecarClient::start_for_profile(command(ECHO_SCRIPT), &paths, Duration::from_secs(1))
                .await
                .expect("first profile sidecar");
        let second =
            SidecarClient::start_for_profile(command(ECHO_SCRIPT), &paths, Duration::from_secs(1))
                .await;
        assert_eq!(second.unwrap_err().kind(), SidecarErrorKind::ProfileInUse);
        first.shutdown().await.expect("first shutdown");
        let mut third =
            SidecarClient::start_for_profile(command(ECHO_SCRIPT), &paths, Duration::from_secs(1))
                .await
                .expect("reopen profile sidecar");
        third.shutdown().await.expect("third shutdown");
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[tokio::test]
    async fn hello_protocol_mismatch_fails_start() {
        let script = r#"process.stdin.on('data',()=>process.stdout.write('{"id":"request-1","ok":true,"result":{"protocolVersion":2}}\n'));"#;
        let error = SidecarClient::start(command(script), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::ProtocolMismatch);
    }

    #[tokio::test]
    async fn silent_request_times_out_and_fails() {
        let script = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');});
"#;
        let mut client = SidecarClient::start(command(script), Duration::from_millis(100))
            .await
            .expect("start");
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::Timeout);
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn child_exit_maps_to_sidecar_exited() {
        let script = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');setTimeout(()=>process.exit(0),20);}});
"#;
        let mut client = SidecarClient::start(command(script), Duration::from_secs(1))
            .await
            .expect("start");
        tokio::time::sleep(Duration::from_millis(80)).await;
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::SidecarExited);
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn hello_without_lf_then_exit_is_not_ready() {
        let script = r#"process.stdin.on('data',()=>{process.stdout.write('{"id":"request-1","ok":true,"result":{"protocolVersion":1}}');process.exit(0);});"#;
        let error = SidecarClient::start(command(script), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::InvalidResponse);
    }

    #[tokio::test]
    async fn ordinary_response_without_lf_then_exit_is_fatal() {
        let script = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else {process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{}}));process.exit(0);}});
"#;
        let mut client = SidecarClient::start(command(script), Duration::from_secs(1))
            .await
            .expect("start");
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::InvalidResponse);
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn invalid_utf8_response_is_fatal_and_redacted() {
        let script = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else process.stdout.write(Buffer.from([123,34,105,100,34,58,34,114,101,113,117,101,115,116,45,50,34,44,34,111,107,34,58,116,114,117,101,44,34,114,101,115,117,108,116,34,58,34,0xc3,0x28,34,125,10]));});
"#;
        let mut client = SidecarClient::start(command(script), Duration::from_secs(1))
            .await
            .expect("start");
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::InvalidResponse);
        assert_eq!(error.to_string(), "兼容组件响应无效");
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn delimiter_free_oversized_response_fails_promptly() {
        let script = format!(
            "const rl=require('readline').createInterface({{input:process.stdin}});rl.on('line',line=>{{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({{id:r.id,ok:true,result:{{protocolVersion:1}}}})+'\\n');else process.stdout.write('x'.repeat({}));}});",
            MAX_FRAME_BYTES
        );
        let mut client = SidecarClient::start(command(&script), Duration::from_secs(5))
            .await
            .expect("start");
        let started = std::time::Instant::now();
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::FrameTooLarge);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn oversized_response_maps_to_frame_too_large() {
        let script = format!(
            "const rl=require('readline').createInterface({{input:process.stdin}});rl.on('line',line=>{{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({{id:r.id,ok:true,result:{{protocolVersion:1}}}})+'\\n');else process.stdout.write('x'.repeat({})+'\\n');}});",
            MAX_FRAME_BYTES + 1
        );
        let mut client = SidecarClient::start(command(&script), Duration::from_secs(2))
            .await
            .expect("start");
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::FrameTooLarge);
        assert_eq!(client.state(), SidecarState::Failed);
    }

    fn exact_boundary_script(target_payload_bytes: usize) -> String {
        format!(
            "const rl=require('readline').createInterface({{input:process.stdin}});const make=(id)=>{{const o={{id,ok:true,result:{{padding:''}}}};const empty=JSON.stringify(o);o.result.padding='x'.repeat({}-Buffer.byteLength(empty));return JSON.stringify(o);}};rl.on('line',line=>{{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({{id:r.id,ok:true,result:{{protocolVersion:1}}}})+'\\n');else if(r.command==='shutdown'){{process.stdout.write(JSON.stringify({{id:r.id,ok:true,result:{{stopped:true}}}})+'\\n');process.exit(0);}}else process.stdout.write(make(r.id)+'\\n');}});",
            target_payload_bytes
        )
    }

    #[tokio::test]
    async fn exact_max_minus_one_payload_plus_lf_is_accepted() {
        let mut client = SidecarClient::start(
            command(&exact_boundary_script(MAX_FRAME_BYTES - 1)),
            Duration::from_secs(5),
        )
        .await
        .expect("start");
        let result = client
            .request_value("echo", json!({}))
            .await
            .expect("boundary response");
        assert!(result["padding"].as_str().expect("padding").len() > MAX_FRAME_BYTES - 100);
        client.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn exact_max_payload_plus_lf_is_rejected() {
        let mut client = SidecarClient::start(
            command(&exact_boundary_script(MAX_FRAME_BYTES)),
            Duration::from_secs(5),
        )
        .await
        .expect("start");
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::FrameTooLarge);
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn exact_max_minus_one_payload_plus_crlf_is_rejected() {
        let script = format!(
            "const rl=require('readline').createInterface({{input:process.stdin}});const make=(id)=>{{const o={{id,ok:true,result:{{padding:''}}}};const empty=JSON.stringify(o);o.result.padding='x'.repeat({}-Buffer.byteLength(empty));return JSON.stringify(o);}};rl.on('line',line=>{{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({{id:r.id,ok:true,result:{{protocolVersion:1}}}})+'\\n');else process.stdout.write(make(r.id)+'\\r\\n');}});",
            MAX_FRAME_BYTES - 1
        );
        let mut client = SidecarClient::start(command(&script), Duration::from_secs(5))
            .await
            .expect("start");
        let error = client.request_value("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::FrameTooLarge);
        assert_eq!(client.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn shutdown_reaps_child() {
        let mut client = SidecarClient::start(command(ECHO_SCRIPT), Duration::from_secs(1))
            .await
            .expect("start");
        client.shutdown().await.expect("shutdown");
        assert_eq!(client.state(), SidecarState::Stopped);
        assert!(client.child.try_wait().expect("wait status").is_some());
    }

    #[tokio::test]
    async fn shutdown_budget_covers_silent_response_and_reap() {
        let script = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');});
"#;
        let mut client = SidecarClient::start(command(script), Duration::from_secs(10))
            .await
            .expect("start");
        let started = std::time::Instant::now();
        let error = client
            .shutdown_with_budget(Duration::from_millis(250))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_millis(400));
        assert_eq!(client.state(), SidecarState::Stopped);
        assert!(client.child.try_wait().expect("wait status").is_some());
    }

    #[tokio::test]
    async fn dropping_live_client_does_not_leave_marker() {
        let marker =
            std::env::temp_dir().join(format!("joplin-sidecar-drop-{}", std::process::id()));
        let pid_path =
            std::env::temp_dir().join(format!("joplin-sidecar-pid-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&pid_path);
        let script = "const fs=require('fs');const p=process.argv[1];const pid=process.argv[2];setInterval(()=>{},1000);const rl=require('readline').createInterface({input:process.stdin});rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello'){fs.writeFileSync(pid,String(process.pid));process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\\n');setTimeout(()=>fs.writeFileSync(p,'survived'),300);}});".to_string();
        let mut args_command = command(&script);
        args_command
            .args
            .push(marker.to_string_lossy().into_owned());
        args_command
            .args
            .push(pid_path.to_string_lossy().into_owned());
        let client = SidecarClient::start(args_command, Duration::from_secs(1))
            .await
            .expect("start");
        let pid: libc::pid_t = std::fs::read_to_string(&pid_path)
            .expect("child pid")
            .parse()
            .expect("numeric pid");
        drop(client);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        let mut observed_esrch = false;
        while tokio::time::Instant::now() < deadline {
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                observed_esrch = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut cleanup_esrch = observed_esrch;
        if !observed_esrch {
            let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            let cleanup_deadline = tokio::time::Instant::now() + Duration::from_millis(300);
            while tokio::time::Instant::now() < cleanup_deadline {
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    cleanup_esrch = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        let marker_present = marker.exists();
        let _ = std::fs::remove_file(marker);
        let _ = std::fs::remove_file(pid_path);
        eprintln!(
            "drop evidence pid={} observed_esrch={} cleanup_esrch={}",
            pid, observed_esrch, cleanup_esrch
        );
        assert!(cleanup_esrch, "cleanup did not reach ESRCH");
        assert!(observed_esrch, "child did not disappear with ESRCH");
        assert!(!marker_present, "child continued running after drop");
    }
}

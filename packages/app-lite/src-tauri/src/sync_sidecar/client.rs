use std::{process::Stdio, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    process::{Child, Command},
    time::timeout,
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

use super::protocol::{
    MAX_FRAME_BYTES, ResponseFrame, STARTUP_TIMEOUT, SidecarCommand, SidecarError,
    SidecarErrorKind, SidecarState, classify_response_error, decode_response, request_frame,
    validate_hello, validate_response_id,
};

type SidecarReader = FramedRead<tokio::process::ChildStdout, LinesCodec>;
type SidecarWriter = FramedWrite<tokio::process::ChildStdin, LinesCodec>;

#[derive(Debug)]
pub struct SidecarClient {
    child: Child,
    reader: SidecarReader,
    writer: SidecarWriter,
    state: SidecarState,
    request_timeout: Duration,
    next_request: u64,
}

impl SidecarClient {
    pub async fn start(
        command: SidecarCommand,
        request_timeout: Duration,
    ) -> Result<Self, SidecarError> {
        let mut process = Command::new(&command.executable);
        process
            .args(&command.args)
            .current_dir(&command.current_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = process
            .spawn()
            .map_err(|error| SidecarError::new(SidecarErrorKind::SpawnFailed, error))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SidecarError::new(SidecarErrorKind::Io, "sidecar stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SidecarError::new(SidecarErrorKind::Io, "sidecar stdout unavailable"))?;
        let mut client = Self {
            child,
            reader: FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
            writer: FramedWrite::new(stdin, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
            state: SidecarState::Starting,
            request_timeout,
            next_request: 2,
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

    pub async fn request(&mut self, command: &str, params: Value) -> Result<Value, SidecarError> {
        if self.state != SidecarState::Ready {
            return Err(SidecarError::new(
                SidecarErrorKind::InvalidResponse,
                "sidecar is not ready",
            ));
        }
        let id = format!("request-{}", self.next_request);
        self.next_request += 1;
        let exchange = timeout(self.request_timeout, self.exchange(&id, command, params)).await;
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
            self.fail(error.kind()).await;
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

    pub fn state(&self) -> SidecarState {
        self.state
    }

    pub async fn shutdown(&mut self) -> Result<(), SidecarError> {
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
        let shutdown_result = timeout(
            Duration::from_secs(5),
            self.exchange(&id, "shutdown", json!({})),
        )
        .await;
        let result = match shutdown_result {
            Ok(Ok(response)) => {
                if validate_response_id(&response, &id).is_err() || !response.ok {
                    Err(SidecarError::new(
                        SidecarErrorKind::InvalidResponse,
                        "shutdown response",
                    ))
                } else {
                    Ok(())
                }
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(SidecarError::new(
                SidecarErrorKind::Timeout,
                "shutdown timeout",
            )),
        };
        let _ = timeout(Duration::from_secs(5), self.child.wait()).await;
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        self.state = SidecarState::Stopped;
        result
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
            return Err(self.map_codec_error(error));
        }
        match self.reader.next().await {
            Some(Ok(line)) => {
                if line.len() + 1 > MAX_FRAME_BYTES {
                    return Err(SidecarError::new(
                        SidecarErrorKind::FrameTooLarge,
                        "response too large",
                    ));
                }
                decode_response(&line)
            }
            Some(Err(error)) => Err(self.map_codec_error(error)),
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
    }

    fn map_codec_error(&mut self, error: LinesCodecError) -> SidecarError {
        if matches!(error, LinesCodecError::Io(_)) && self.child.try_wait().ok().flatten().is_some()
        {
            SidecarError::new(SidecarErrorKind::SidecarExited, "sidecar exited")
        } else {
            map_codec_error(error)
        }
    }
}

fn map_codec_error(error: LinesCodecError) -> SidecarError {
    match error {
        LinesCodecError::MaxLineLengthExceeded => {
            SidecarError::new(SidecarErrorKind::FrameTooLarge, "frame too large")
        }
        LinesCodecError::Io(_) => SidecarError::new(SidecarErrorKind::Io, "codec io"),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use serde_json::json;

    use super::*;

    fn command(script: &str) -> SidecarCommand {
        SidecarCommand {
            executable: PathBuf::from("node"),
            args: vec!["-e".into(), script.into()],
            current_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        }
    }

    const ECHO_SCRIPT: &str = r#"
const rl=require('readline').createInterface({input:process.stdin});
rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello')process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\n');else if(r.command==='shutdown'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{}})+'\n');process.exit(0);}else process.stdout.write(JSON.stringify({id:r.id,ok:true,result:r.params})+'\n');});
"#;

    #[tokio::test]
    async fn valid_hello_and_echo_are_correlated() {
        let mut client = SidecarClient::start(command(ECHO_SCRIPT), Duration::from_secs(1))
            .await
            .expect("sidecar starts");
        assert_eq!(client.state(), SidecarState::Ready);
        let result = client
            .request("echo", json!({"value":"ok"}))
            .await
            .expect("echo");
        assert_eq!(result["value"], "ok");
        client.shutdown().await.expect("shutdown");
        assert_eq!(client.state(), SidecarState::Stopped);
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
        let error = client.request("echo", json!({})).await.unwrap_err();
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
        let error = client.request("echo", json!({})).await.unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::SidecarExited);
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
        let error = client.request("echo", json!({})).await.unwrap_err();
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
    async fn dropping_live_client_does_not_leave_marker() {
        let marker =
            std::env::temp_dir().join(format!("joplin-sidecar-drop-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let script = "const fs=require('fs');const p=process.argv[1];const rl=require('readline').createInterface({input:process.stdin});rl.on('line',line=>{const r=JSON.parse(line);if(r.command==='hello'){process.stdout.write(JSON.stringify({id:r.id,ok:true,result:{protocolVersion:1}})+'\\n');setTimeout(()=>fs.writeFileSync(p,'survived'),300);}});".to_string();
        let mut args_command = command(&script);
        args_command
            .args
            .push(marker.to_string_lossy().into_owned());
        let client = SidecarClient::start(args_command, Duration::from_secs(1))
            .await
            .expect("start");
        drop(client);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!marker.exists());
        let _ = std::fs::remove_file(marker);
    }
}

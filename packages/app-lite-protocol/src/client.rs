//! Blocking HTTP client for the routes in docs/research/sync-client-design-v1.md §1.

use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use crate::{
    BlobStatus, EVENTS_HEARTBEAT_SECONDS, PullRequest, PullResponse, PushRequest, PushResponse,
    SyncEvent, SyncTransport, TransportError,
};

/// No byte for this long means the event stream is dead: after a network
/// change a half-open connection reports no error of its own. Two missed
/// heartbeats plus slack.
pub const EVENTS_SILENCE_TIMEOUT: Duration = Duration::from_secs(EVENTS_HEARTBEAT_SECONDS * 5 / 2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_EVENT_LINE_BYTES: u64 = 4096;

/// Upper bound on a response body the client will read.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

pub struct HttpTransport {
    base_url: String,
    authorization: String,
    agent: ureq::Agent,
}

impl HttpTransport {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            authorization: format!("Bearer {token}"),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout(Duration::from_secs(120))
                .build(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    fn body(response: ureq::Response) -> Result<Vec<u8>, TransportError> {
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| TransportError::Retryable(error.to_string()))?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(TransportError::Permanent("response too large".into()));
        }
        Ok(bytes)
    }

    fn json<T: serde::de::DeserializeOwned>(
        result: Result<ureq::Response, ureq::Error>,
    ) -> Result<T, TransportError> {
        let bytes = Self::body(Self::checked(result)?)?;
        serde_json::from_slice(&bytes).map_err(|error| TransportError::Permanent(error.to_string()))
    }

    fn checked(
        result: Result<ureq::Response, ureq::Error>,
    ) -> Result<ureq::Response, TransportError> {
        match result {
            Ok(response) => Ok(response),
            Err(ureq::Error::Status(401, _)) => Err(TransportError::Unauthorized),
            Err(ureq::Error::Status(409, response)) => {
                let bytes = Self::body(response)?;
                #[derive(serde::Deserialize)]
                struct Expected {
                    expected: u64,
                }
                serde_json::from_slice::<Expected>(&bytes)
                    .map(|body| TransportError::OffsetMismatch {
                        expected: body.expected,
                    })
                    .map_err(|error| TransportError::Permanent(error.to_string()))
                    .and_then(Err)
            }
            Err(ureq::Error::Status(status, response)) if status < 500 => {
                let reason = response.into_string().unwrap_or_default();
                Err(TransportError::Permanent(format!(
                    "HTTP {status}: {reason}"
                )))
            }
            Err(ureq::Error::Status(status, _)) => {
                Err(TransportError::Retryable(format!("HTTP {status}")))
            }
            Err(error) => Err(TransportError::Retryable(error.to_string())),
        }
    }

    fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<ureq::Response, ureq::Error> {
        let bytes = serde_json::to_vec(body).expect("protocol types serialize");
        self.agent
            .post(&self.url(path))
            .set("Authorization", &self.authorization)
            .set("Content-Type", "application/json")
            .send_bytes(&bytes)
    }
}

impl HttpTransport {
    /// Opens `GET /v1/events`; see `EVENTS_SILENCE_TIMEOUT`.
    pub fn open_events(&self) -> Result<EventStream, TransportError> {
        self.open_events_with(EVENTS_SILENCE_TIMEOUT)
    }

    pub fn open_events_with(&self, silence: Duration) -> Result<EventStream, TransportError> {
        // Its own agent: the request timeout of `agent` would cut a healthy
        // stream, while a per-read timeout is exactly the silence rule.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(silence)
            .timeout_write(CONNECT_TIMEOUT)
            .build();
        let response = Self::checked(
            agent
                .get(&self.url("/v1/events"))
                .set("Authorization", &self.authorization)
                .set("Accept", "text/event-stream")
                .set("Connection", "close")
                .call(),
        )?;
        Ok(EventStream {
            lines: BufReader::new(Box::new(response.into_reader())),
        })
    }
}

pub struct EventStream {
    lines: BufReader<Box<dyn Read + Send + Sync>>,
}

impl EventStream {
    /// Blocks for the next event. Heartbeat comments only keep the stream
    /// alive. Silence, a closed connection, or an oversized line is
    /// `Retryable`: reconnect.
    pub fn next_event(&mut self) -> Result<SyncEvent, TransportError> {
        let (mut event, mut data) = (String::new(), String::new());
        loop {
            let mut line = String::new();
            let read = (&mut self.lines)
                .take(MAX_EVENT_LINE_BYTES)
                .read_line(&mut line)
                .map_err(|error| TransportError::Retryable(format!("event stream: {error}")))?;
            if read == 0 {
                return Err(TransportError::Retryable("event stream closed".into()));
            }
            if !line.ends_with('\n') {
                return Err(TransportError::Retryable("event line too long".into()));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if let Some(parsed) = SyncEvent::from_sse(&event, &data) {
                    return Ok(parsed);
                }
                event.clear();
                data.clear();
            } else if let Some(value) = line.strip_prefix("event:") {
                event = value.trim().to_owned();
            } else if let Some(value) = line.strip_prefix("data:") {
                data = value.trim().to_owned();
            }
        }
    }
}

impl SyncTransport for HttpTransport {
    fn push(&self, request: &PushRequest) -> Result<PushResponse, TransportError> {
        Self::json(self.post("/v1/push", request))
    }

    fn pull(&self, request: &PullRequest) -> Result<PullResponse, TransportError> {
        Self::json(self.post("/v1/pull", request))
    }

    fn blob_status(&self, sha256: &str) -> Result<BlobStatus, TransportError> {
        Self::json(
            self.agent
                .get(&self.url(&format!("/v1/blobs/{sha256}")))
                .set("Authorization", &self.authorization)
                .call(),
        )
    }

    fn put_chunk(
        &self,
        sha256: &str,
        size: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<BlobStatus, TransportError> {
        Self::json(
            self.agent
                .put(&self.url(&format!("/v1/blobs/{sha256}")))
                .query("size", &size.to_string())
                .query("offset", &offset.to_string())
                .set("Authorization", &self.authorization)
                .set("Content-Type", "application/octet-stream")
                .send_bytes(bytes),
        )
    }

    fn read_range(&self, sha256: &str, offset: u64, len: u64) -> Result<Vec<u8>, TransportError> {
        let response = Self::checked(
            self.agent
                .get(&self.url(&format!("/v1/blobs/{sha256}/range")))
                .query("offset", &offset.to_string())
                .query("len", &len.to_string())
                .set("Authorization", &self.authorization)
                .call(),
        )?;
        Self::body(response)
    }
}

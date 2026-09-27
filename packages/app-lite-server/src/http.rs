//! HTTP routes for protocol v1 (docs/research/sync-client-design-v1.md §1):
//! bearer auth on every request, bounded bodies, `ServerError` mapped to
//! status codes the client classifies as retryable or permanent.
//!
//! A minimal HTTP/1.1 server of our own rather than a library: every
//! connection gets read and write timeouts, so a client that vanished
//! mid-request (a phone losing its radio, an expired carrier NAT entry)
//! frees its thread instead of holding it forever. One request per
//! connection, `Content-Length` bodies only; `GET /v1/events` streams
//! Server-Sent Events on the same connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use app_lite_protocol::{EVENTS_HEARTBEAT_SECONDS, EVENTS_MAX_STREAM_SECONDS, SyncEvent};

use crate::{ServerError, ServerStore};

pub const MAX_JSON_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
pub const MIN_TOKEN_BYTES: usize = 32;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const LINGER: Duration = Duration::from_secs(2);
const MAX_LINGER_BYTES: u64 = 2 * MAX_JSON_BODY_BYTES;

/// Timing and limits that tests shorten; production uses `Default`.
#[derive(Clone, Copy, Debug)]
pub struct HttpOptions {
    /// Longest wait for bytes from, or room to send to, one client.
    pub socket_timeout: Duration,
    pub heartbeat: Duration,
    pub max_stream: Duration,
    /// Event streams each hold a connection for their lifetime.
    pub max_streams: usize,
    /// All open connections, streams included.
    pub max_connections: usize,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            socket_timeout: Duration::from_secs(60),
            heartbeat: Duration::from_secs(EVENTS_HEARTBEAT_SECONDS),
            max_stream: Duration::from_secs(EVENTS_MAX_STREAM_SECONDS),
            max_streams: 32,
            max_connections: 64,
        }
    }
}

struct Shared {
    store: Arc<ServerStore>,
    token: String,
    options: HttpOptions,
    stopping: AtomicBool,
    connections: AtomicUsize,
    streams: AtomicUsize,
}

/// A bound server; dropping it stops accepting. Open connections finish
/// their request; event streams end within one heartbeat.
pub struct HttpServer {
    address: SocketAddr,
    shared: Arc<Shared>,
    acceptor: Option<JoinHandle<()>>,
}

impl HttpServer {
    pub fn bind(
        address: &str,
        store: Arc<ServerStore>,
        token: String,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::bind_with(address, store, token, HttpOptions::default())
    }

    pub fn bind_with(
        address: &str,
        store: Arc<ServerStore>,
        token: String,
        options: HttpOptions,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if token.len() < MIN_TOKEN_BYTES {
            return Err(format!("token must be at least {MIN_TOKEN_BYTES} bytes").into());
        }
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            store,
            token,
            options,
            stopping: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            streams: AtomicUsize::new(0),
        });
        let acceptor = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || accept_loop(&listener, &shared))
        };
        Ok(Self {
            address,
            shared,
            acceptor: Some(acceptor),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        // Wake the blocking accept.
        let _ = TcpStream::connect(self.address);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

/// Decrements a counter however the holder ends.
struct Slot<'a>(&'a AtomicUsize);

impl<'a> Slot<'a> {
    fn claim(counter: &'a AtomicUsize, limit: usize) -> Option<Self> {
        if counter.fetch_add(1, Ordering::AcqRel) >= limit {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(counter))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    for socket in listener.incoming() {
        if shared.stopping.load(Ordering::Acquire) {
            return;
        }
        let Ok(socket) = socket else { continue };
        let timeout = Some(shared.options.socket_timeout);
        if socket.set_read_timeout(timeout).is_err() || socket.set_write_timeout(timeout).is_err() {
            continue;
        }
        let _ = socket.set_nodelay(true);
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let Some(_slot) = Slot::claim(&shared.connections, shared.options.max_connections)
            else {
                let _ = write_reply(&socket, text(503, "server busy"));
                return;
            };
            serve(&shared, socket);
        });
    }
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

fn text(status: u16, message: &str) -> Reply {
    Reply {
        status,
        content_type: "text/plain; charset=utf-8",
        body: message.as_bytes().to_vec(),
    }
}

fn json(value: &impl serde::Serialize) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: serde_json::to_vec(value).expect("protocol types serialize"),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

/// After a reply the client may still be sending a body the server
/// refused; closing then would reset the connection before the client reads
/// the reply. Read and discard for a bounded time and amount first.
fn linger_close(socket: &TcpStream, options: &HttpOptions) {
    let linger = LINGER.min(options.socket_timeout);
    let _ = socket.shutdown(std::net::Shutdown::Write);
    let _ = socket.set_read_timeout(Some(linger));
    let started = Instant::now();
    let mut sink = [0u8; 64 * 1024];
    let mut discarded = 0u64;
    let mut reader = socket;
    while started.elapsed() < linger && discarded < MAX_LINGER_BYTES {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => return,
            Ok(count) => discarded += count as u64,
        }
    }
}

/// JSON replies above this go gzip-compressed to clients that accept it;
/// attachment bytes are mostly compressed already.
const GZIP_REPLY_ABOVE_BYTES: usize = 1024;

fn write_reply(socket: &TcpStream, reply: Reply) -> std::io::Result<()> {
    write_reply_encoded(socket, reply, false)
}

fn write_reply_encoded(
    mut socket: &TcpStream,
    mut reply: Reply,
    accepts_gzip: bool,
) -> std::io::Result<()> {
    let mut encoding = "";
    if accepts_gzip
        && reply.content_type == "application/json"
        && reply.body.len() > GZIP_REPLY_ABOVE_BYTES
    {
        let mut encoder = flate2::write::GzEncoder::new(
            Vec::with_capacity(reply.body.len() / 4),
            flate2::Compression::default(),
        );
        encoder.write_all(&reply.body)?;
        reply.body = encoder.finish()?;
        encoding = "Content-Encoding: gzip\r\n";
    }
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n{encoding}Content-Length: {}\r\nConnection: close\r\n\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type,
        reply.body.len()
    );
    socket.write_all(head.as_bytes())?;
    socket.write_all(&reply.body)?;
    socket.flush()
}

fn error(error: ServerError) -> Reply {
    match error {
        ServerError::OffsetMismatch { expected } => Reply {
            status: 409,
            ..json(&serde_json::json!({ "expected": expected }))
        },
        ServerError::Protocol(_) | ServerError::BadRequest(_) | ServerError::Json(_) => {
            text(400, &error.to_string())
        }
        ServerError::HashMismatch => text(422, &error.to_string()),
        ServerError::NotFound => text(404, &error.to_string()),
        // Details stay in the server; the client only needs to retry.
        ServerError::Storage(_) | ServerError::Io(_) => text(500, "storage error"),
    }
}

struct Request {
    method: String,
    url: String,
    authorization: Option<String>,
    content_length: Option<u64>,
    gzip_body: bool,
    accepts_gzip: bool,
}

impl Request {
    fn path(&self) -> &str {
        self.url.split('?').next().unwrap_or_default()
    }
}

/// Request line and headers; the body stays in `reader`.
fn read_head(reader: &mut BufReader<&TcpStream>) -> Result<Request, Reply> {
    let mut read = 0;
    let mut line = |reader: &mut BufReader<&TcpStream>| -> Result<String, Reply> {
        let mut line = String::new();
        let limit = (MAX_HEADER_BYTES - read) as u64 + 1;
        let count = reader
            .take(limit)
            .read_line(&mut line)
            .map_err(|_| text(400, "unreadable request"))?;
        read += count;
        if read > MAX_HEADER_BYTES {
            return Err(text(431, "request headers too large"));
        }
        if count == 0 {
            return Err(text(400, "incomplete request"));
        }
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    };
    let request_line = line(reader)?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(url), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(text(400, "malformed request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(text(400, "unsupported HTTP version"));
    }
    let mut request = Request {
        method: method.to_owned(),
        url: url.to_owned(),
        authorization: None,
        content_length: None,
        gzip_body: false,
        accepts_gzip: false,
    };
    loop {
        let header = line(reader)?;
        if header.is_empty() {
            return Ok(request);
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(text(400, "malformed header"));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            request.authorization = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            request.content_length =
                Some(value.parse().map_err(|_| text(400, "bad content-length"))?);
        } else if name.eq_ignore_ascii_case("content-encoding") {
            match value.to_ascii_lowercase().as_str() {
                "gzip" => request.gzip_body = true,
                "identity" => {}
                _ => return Err(text(415, "only gzip request bodies are accepted")),
            }
        } else if name.eq_ignore_ascii_case("accept-encoding") {
            request.accepts_gzip = value
                .split(',')
                .any(|coding| coding.trim().to_ascii_lowercase().starts_with("gzip"));
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(text(501, "only Content-Length bodies are accepted"));
        }
    }
}

/// Constant-time for equal lengths; the token length is not secret.
fn authorized(request: &Request, token: &str) -> bool {
    let expected = format!("Bearer {token}");
    request.authorization.as_deref().is_some_and(|given| {
        let given = given.as_bytes();
        given.len() == expected.len()
            && given
                .iter()
                .zip(expected.as_bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    })
}

fn body(
    reader: &mut BufReader<&TcpStream>,
    request: &Request,
    limit: u64,
) -> Result<Vec<u8>, Reply> {
    let Some(length) = request.content_length else {
        return Err(text(411, "Content-Length is required"));
    };
    if length > limit {
        return Err(text(413, "request body too large"));
    }
    let mut bytes = vec![0; length as usize];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| text(400, "unreadable request body"))?;
    if !request.gzip_body {
        return Ok(bytes);
    }
    // The limit applies to the decompressed size as well.
    let mut plain = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .take(limit + 1)
        .read_to_end(&mut plain)
        .map_err(|_| text(400, "malformed gzip body"))?;
    if plain.len() as u64 > limit {
        return Err(text(413, "request body too large"));
    }
    Ok(plain)
}

fn query(url: &str, name: &str) -> Option<u64> {
    url.split_once('?')?
        .1
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .and_then(|(_, value)| value.parse().ok())
}

fn serve(shared: &Shared, socket: TcpStream) {
    let mut reader = BufReader::new(&socket);
    let request = match read_head(&mut reader) {
        Ok(request) => request,
        Err(reply) => {
            let _ = write_reply(&socket, reply);
            linger_close(&socket, &shared.options);
            return;
        }
    };
    if !authorized(&request, &shared.token) {
        let _ = write_reply(&socket, text(401, "unauthorized"));
        linger_close(&socket, &shared.options);
        return;
    }
    if request.method == "GET" && request.path() == "/v1/events" {
        match Slot::claim(&shared.streams, shared.options.max_streams) {
            Some(_slot) => {
                let _ = stream_events(shared, &socket);
            }
            None => {
                let _ = write_reply(&socket, text(503, "too many event streams"));
            }
        }
        return;
    }
    let reply = route(&shared.store, &request, &mut reader).unwrap_or_else(|reply| reply);
    let _ = write_reply_encoded(&socket, reply, request.accepts_gzip);
    linger_close(&socket, &shared.options);
}

/// `hello`, then `changed` whenever the head moves and a comment line when
/// quiet, until the client goes away, the server stops, or the stream has
/// run `max_stream` (then `bye`).
fn stream_events(shared: &Shared, mut socket: &TcpStream) -> std::io::Result<()> {
    socket.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
          X-Accel-Buffering: no\r\nConnection: close\r\n\r\n",
    )?;
    let mut seen = shared.store.head();
    socket.write_all(SyncEvent::Hello { head: seen }.to_sse().as_bytes())?;
    socket.flush()?;
    let started = Instant::now();
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(remaining) = shared.options.max_stream.checked_sub(started.elapsed()) else {
            socket.write_all(SyncEvent::Bye.to_sse().as_bytes())?;
            return socket.flush();
        };
        let head = shared
            .store
            .wait_for_head(seen, shared.options.heartbeat.min(remaining));
        if head != seen {
            seen = head;
            socket.write_all(SyncEvent::Changed { head }.to_sse().as_bytes())?;
        } else {
            socket.write_all(b": ping\n\n")?;
        }
        socket.flush()?;
    }
}

fn route(
    store: &ServerStore,
    request: &Request,
    reader: &mut BufReader<&TcpStream>,
) -> Result<Reply, Reply> {
    let segments: Vec<&str> = request.path().trim_matches('/').split('/').collect();
    let url = request.url.as_str();
    Ok(match (request.method.as_str(), segments.as_slice()) {
        ("POST", ["v1", "push"]) => {
            let bytes = body(reader, request, MAX_JSON_BODY_BYTES)?;
            match serde_json::from_slice(&bytes) {
                Ok(push) => store.push(push).map(|response| json(&response)),
                Err(parse) => Err(ServerError::Json(parse)),
            }
            .unwrap_or_else(error)
        }
        ("POST", ["v1", "pull"]) => {
            let bytes = body(reader, request, MAX_JSON_BODY_BYTES)?;
            match serde_json::from_slice(&bytes) {
                Ok(pull) => store.pull(pull).map(|response| json(&response)),
                Err(parse) => Err(ServerError::Json(parse)),
            }
            .unwrap_or_else(error)
        }
        ("GET", ["v1", "blobs", sha256]) => store
            .blob_status(sha256)
            .map(|status| json(&status))
            .unwrap_or_else(error),
        ("PUT", ["v1", "blobs", sha256]) => match (query(url, "size"), query(url, "offset")) {
            (Some(size), Some(offset)) => {
                let bytes = body(reader, request, MAX_CHUNK_BYTES)?;
                store
                    .put_chunk(sha256, size, offset, &bytes)
                    .map(|status| json(&status))
                    .unwrap_or_else(error)
            }
            _ => text(400, "size and offset are required"),
        },
        ("GET", ["v1", "blobs", sha256, "range"]) => {
            match (query(url, "offset"), query(url, "len")) {
                (Some(offset), Some(len)) if len <= MAX_CHUNK_BYTES => store
                    .read_range(sha256, offset, len)
                    .map(|bytes| Reply {
                        status: 200,
                        content_type: "application/octet-stream",
                        body: bytes,
                    })
                    .unwrap_or_else(error),
                (Some(_), Some(_)) => text(413, "range too large"),
                _ => text(400, "offset and len are required"),
            }
        }
        _ => text(404, "no such route"),
    })
}

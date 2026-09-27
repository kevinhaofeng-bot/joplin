//! HTTP routes for protocol v1 (docs/research/sync-client-design-v1.md §1):
//! bearer auth on every request, bounded bodies, `ServerError` mapped to
//! status codes the client classifies as retryable or permanent.

use std::io::{Cursor, Read};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use tiny_http::{Header, Method, Request, Response, Server};

use crate::{ServerError, ServerStore};

pub const MAX_JSON_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
pub const MIN_TOKEN_BYTES: usize = 32;
const WORKERS: usize = 4;

/// A bound server with a small worker pool; dropping it stops the workers.
pub struct HttpServer {
    server: Arc<Server>,
    stopping: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl HttpServer {
    pub fn bind(
        address: &str,
        store: Arc<ServerStore>,
        token: String,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if token.len() < MIN_TOKEN_BYTES {
            return Err(format!("token must be at least {MIN_TOKEN_BYTES} bytes").into());
        }
        let server = Arc::new(Server::http(address)?);
        let stopping = Arc::new(AtomicBool::new(false));
        let token = Arc::new(token);
        let workers = (0..WORKERS)
            .map(|_| {
                let server = Arc::clone(&server);
                let stopping = Arc::clone(&stopping);
                let store = Arc::clone(&store);
                let token = Arc::clone(&token);
                std::thread::spawn(move || {
                    while !stopping.load(Ordering::Acquire) {
                        if let Ok(request) = server.recv() {
                            handle(&store, &token, request);
                        }
                    }
                })
            })
            .collect();
        Ok(Self {
            server,
            stopping,
            workers,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.server
            .server_addr()
            .to_ip()
            .expect("bound to an IP address")
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        for _ in &self.workers {
            self.server.unblock();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

type Reply = Response<Cursor<Vec<u8>>>;

fn handle(store: &ServerStore, token: &str, mut request: Request) {
    let reply = route(store, token, &mut request);
    let _ = request.respond(reply);
}

fn text(status: u16, message: &str) -> Reply {
    Response::from_string(message).with_status_code(status)
}

fn json(value: &impl serde::Serialize) -> Reply {
    Response::from_data(serde_json::to_vec(value).expect("protocol types serialize"))
        .with_header(Header::from_bytes("Content-Type", "application/json").expect("valid header"))
}

fn error(error: ServerError) -> Reply {
    match error {
        ServerError::OffsetMismatch { expected } => {
            json(&serde_json::json!({ "expected": expected })).with_status_code(409)
        }
        ServerError::Protocol(_) | ServerError::BadRequest(_) | ServerError::Json(_) => {
            text(400, &error.to_string())
        }
        ServerError::HashMismatch => text(422, &error.to_string()),
        ServerError::NotFound => text(404, &error.to_string()),
        // Details stay in the server; the client only needs to retry.
        ServerError::Storage(_) | ServerError::Io(_) => text(500, "storage error"),
    }
}

/// Constant-time for equal lengths; the token length is not secret.
fn authorized(request: &Request, token: &str) -> bool {
    let expected = format!("Bearer {token}");
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .is_some_and(|header| {
            let given = header.value.as_str().as_bytes();
            given.len() == expected.len()
                && given
                    .iter()
                    .zip(expected.as_bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
        })
}

fn body(request: &mut Request, limit: u64) -> Result<Vec<u8>, Reply> {
    if request
        .body_length()
        .is_some_and(|length| length as u64 > limit)
    {
        return Err(text(413, "request body too large"));
    }
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| text(400, "unreadable request body"))?;
    if bytes.len() as u64 > limit {
        return Err(text(413, "request body too large"));
    }
    Ok(bytes)
}

fn query(url: &str, name: &str) -> Option<u64> {
    url.split_once('?')?
        .1
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .and_then(|(_, value)| value.parse().ok())
}

fn route(store: &ServerStore, token: &str, request: &mut Request) -> Reply {
    if !authorized(request, token) {
        return text(401, "unauthorized");
    }
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or_default();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let method = request.method().clone();
    let outcome = match (method, segments.as_slice()) {
        (Method::Post, ["v1", "push"]) => body(request, MAX_JSON_BODY_BYTES).map(|bytes| {
            match serde_json::from_slice(&bytes) {
                Ok(push) => store.push(push).map(|response| json(&response)),
                Err(parse) => Err(ServerError::Json(parse)),
            }
            .unwrap_or_else(error)
        }),
        (Method::Post, ["v1", "pull"]) => body(request, MAX_JSON_BODY_BYTES).map(|bytes| {
            match serde_json::from_slice(&bytes) {
                Ok(pull) => store.pull(pull).map(|response| json(&response)),
                Err(parse) => Err(ServerError::Json(parse)),
            }
            .unwrap_or_else(error)
        }),
        (Method::Get, ["v1", "blobs", sha256]) => Ok(store
            .blob_status(sha256)
            .map(|status| json(&status))
            .unwrap_or_else(error)),
        (Method::Put, ["v1", "blobs", sha256]) => {
            match (query(&url, "size"), query(&url, "offset")) {
                (Some(size), Some(offset)) => body(request, MAX_CHUNK_BYTES).map(|bytes| {
                    store
                        .put_chunk(sha256, size, offset, &bytes)
                        .map(|status| json(&status))
                        .unwrap_or_else(error)
                }),
                _ => Err(text(400, "size and offset are required")),
            }
        }
        (Method::Get, ["v1", "blobs", sha256, "range"]) => {
            match (query(&url, "offset"), query(&url, "len")) {
                (Some(offset), Some(len)) if len <= MAX_CHUNK_BYTES => Ok(store
                    .read_range(sha256, offset, len)
                    .map(|bytes| {
                        Response::from_data(bytes).with_header(
                            Header::from_bytes("Content-Type", "application/octet-stream")
                                .expect("valid header"),
                        )
                    })
                    .unwrap_or_else(error)),
                (Some(_), Some(_)) => Err(text(413, "range too large")),
                _ => Err(text(400, "offset and len are required")),
            }
        }
        _ => Err(text(404, "no such route")),
    };
    outcome.unwrap_or_else(|reply| reply)
}

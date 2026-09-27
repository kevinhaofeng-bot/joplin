//! `GET /v1/events` and socket timeouts over a real localhost socket, read
//! with a raw client so the wire format itself is checked.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use app_lite_protocol::{
    Action, EntityKind, EntityRef, Operation, PROTOCOL_VERSION, PullRequest, PushRequest,
    SyncTransport, client::HttpTransport,
};
use app_lite_server::{
    ServerStore,
    http::{HttpOptions, HttpServer},
};
use tempfile::tempdir;

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";
const DEVICE: &str = "dddddddddddddddddddddddddddddddd";

struct Running {
    _root: tempfile::TempDir,
    server: HttpServer,
}

impl Running {
    fn url(&self) -> String {
        format!("http://{}", self.server.local_addr())
    }
}

fn start(options: HttpOptions) -> Running {
    let root = tempdir().unwrap();
    let store = Arc::new(ServerStore::open(root.path()).unwrap());
    let server = HttpServer::bind_with("127.0.0.1:0", store, TOKEN.into(), options).unwrap();
    Running {
        _root: root,
        server,
    }
}

fn fast() -> HttpOptions {
    HttpOptions {
        socket_timeout: Duration::from_secs(5),
        heartbeat: Duration::from_millis(200),
        max_stream: Duration::from_secs(2),
        max_streams: 4,
        max_connections: 64,
    }
}

fn push_note(url: &str, op: char) {
    let transport = HttpTransport::new(url, TOKEN);
    transport
        .push(&PushRequest {
            protocol: PROTOCOL_VERSION,
            device_id: DEVICE.into(),
            ops: vec![Operation {
                op_id: op.to_string().repeat(32),
                device_id: DEVICE.into(),
                entity: EntityRef {
                    kind: EntityKind::Note,
                    id: op.to_string().repeat(32),
                },
                base_revision: 0,
                action: Action::Put {
                    payload: serde_json::json!({ "title": "t" }),
                },
            }],
        })
        .unwrap();
}

struct Stream {
    lines: BufReader<TcpStream>,
}

fn open(running: &Running, token: Option<&str>) -> (u16, Stream) {
    let mut socket = TcpStream::connect(running.server.local_addr()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    write!(
        socket,
        "GET /v1/events HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}\r\n"
    )
    .unwrap();
    let mut lines = BufReader::new(socket);
    let mut status = String::new();
    lines.read_line(&mut status).unwrap();
    let code = status.split(' ').nth(1).unwrap().parse().unwrap();
    loop {
        let mut header = String::new();
        lines.read_line(&mut header).unwrap();
        if header == "\r\n" {
            break;
        }
    }
    (code, Stream { lines })
}

impl Stream {
    /// The next record: `("", ": ping")` for a comment, `(event, data)`
    /// otherwise; `("eof", "")` when the server closed the stream.
    fn next(&mut self) -> (String, String) {
        let (mut event, mut data) = (String::new(), String::new());
        loop {
            let mut line = String::new();
            if self.lines.read_line(&mut line).unwrap() == 0 {
                return ("eof".into(), String::new());
            }
            let line = line.trim_end_matches('\n');
            if line.is_empty() {
                return (event, data);
            }
            if line.starts_with(':') {
                data = line.to_owned();
            } else if let Some(value) = line.strip_prefix("event: ") {
                event = value.to_owned();
            } else if let Some(value) = line.strip_prefix("data: ") {
                data = value.to_owned();
            }
        }
    }

    fn next_event(&mut self) -> (String, String) {
        loop {
            let record = self.next();
            if !record.0.is_empty() {
                return record;
            }
        }
    }
}

#[test]
fn a_stream_says_hello_announces_each_push_keeps_the_line_alive_and_ends_on_schedule() {
    let running = start(fast());
    push_note(&running.url(), 'a');
    let (status, mut stream) = open(&running, Some(TOKEN));
    assert_eq!(status, 200);
    assert_eq!(stream.next(), ("hello".into(), r#"{"head":1}"#.into()));
    assert_eq!(stream.next(), ("".into(), ": ping".into()), "heartbeat");
    push_note(&running.url(), 'b');
    assert_eq!(
        stream.next_event(),
        ("changed".into(), r#"{"head":2}"#.into())
    );
    let started = Instant::now();
    assert_eq!(stream.next_event(), ("bye".into(), "{}".into()));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(stream.next().0, "eof");
}

#[test]
fn streams_need_the_token_and_are_bounded_in_number() {
    let running = start(HttpOptions {
        max_streams: 1,
        ..fast()
    });
    assert_eq!(open(&running, None).0, 401);
    let (first, held) = open(&running, Some(TOKEN));
    assert_eq!(first, 200);
    assert_eq!(open(&running, Some(TOKEN)).0, 503);
    drop(held);
    // The server finds the closed socket at its next heartbeat write.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let (status, _) = open(&running, Some(TOKEN));
        if status == 200 {
            break;
        }
        assert!(Instant::now() < deadline, "the slot is released");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn clients_that_go_silent_mid_request_do_not_hold_the_workers() {
    let running = start(HttpOptions {
        socket_timeout: Duration::from_millis(500),
        max_connections: 4,
        ..fast()
    });
    // Stalled uploads fill every connection slot: each announces a body it
    // never finishes, like a phone that lost its radio mid-request.
    let stalled: Vec<TcpStream> = (0..4)
        .map(|_| {
            let mut socket = TcpStream::connect(running.server.local_addr()).unwrap();
            write!(
                socket,
                "POST /v1/pull HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 100000\r\n\r\n{{"
            )
            .unwrap();
            socket
        })
        .collect();
    // Past the socket timeout (plus the bounded lingering close) the slots
    // are free again.
    std::thread::sleep(Duration::from_millis(1500));
    let started = Instant::now();
    let response = HttpTransport::new(&running.url(), TOKEN)
        .pull(&PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: 0,
            limit: 10,
        })
        .unwrap();
    assert!(response.changes.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "served after {:?}",
        started.elapsed()
    );
    drop(stalled);
}

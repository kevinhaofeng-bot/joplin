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
        tls: None,
        socket_timeout: Duration::from_secs(5),
        request_head_timeout: Duration::from_secs(5),
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

/// Forwards bytes both ways until `blackhole` is set; then keeps both
/// sockets open and moves nothing, like a link that died without a FIN or
/// RST (base station handover, expired NAT entry).
struct FaultProxy {
    address: std::net::SocketAddr,
    blackhole: Arc<std::sync::atomic::AtomicBool>,
}

fn fault_proxy(upstream: std::net::SocketAddr) -> FaultProxy {
    use std::sync::atomic::{AtomicBool, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let blackhole = Arc::new(AtomicBool::new(false));
    let hole = Arc::clone(&blackhole);
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { return };
            let server = TcpStream::connect(upstream).unwrap();
            for (mut from, mut to) in [
                (client.try_clone().unwrap(), server.try_clone().unwrap()),
                (server, client),
            ] {
                let hole = Arc::clone(&hole);
                std::thread::spawn(move || {
                    use std::io::Read;
                    from.set_read_timeout(Some(Duration::from_millis(20)))
                        .unwrap();
                    let mut buffer = [0u8; 8192];
                    loop {
                        if hole.load(Ordering::Acquire) {
                            std::thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                        match from.read(&mut buffer) {
                            Ok(0) => return,
                            // Bytes in flight when the link died are lost.
                            Ok(_) if hole.load(Ordering::Acquire) => {}
                            Ok(count) => {
                                if to.write_all(&buffer[..count]).is_err() {
                                    return;
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                            Err(_) => return,
                        }
                    }
                });
            }
        }
    });
    FaultProxy { address, blackhole }
}

#[test]
fn the_client_follows_the_stream_and_calls_a_silent_one_dead() {
    use app_lite_protocol::SyncEvent;
    let running = start(fast());
    let proxy = fault_proxy(running.server.local_addr());
    let through_proxy = HttpTransport::new(&format!("http://{}", proxy.address), TOKEN);
    let mut stream = through_proxy
        .open_events_with(Duration::from_secs(1))
        .unwrap();
    assert_eq!(stream.next_event().unwrap(), SyncEvent::Hello { head: 0 });
    push_note(&running.url(), 'a');
    assert_eq!(stream.next_event().unwrap(), SyncEvent::Changed { head: 1 });

    proxy
        .blackhole
        .store(true, std::sync::atomic::Ordering::Release);
    push_note(&running.url(), 'b');
    let started = Instant::now();
    let silent = stream.next_event();
    assert!(
        matches!(silent, Err(app_lite_protocol::TransportError::Retryable(_))),
        "{silent:?} after {:?}",
        started.elapsed()
    );
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(800) && waited < Duration::from_secs(3),
        "dead after {waited:?}"
    );

    // Once the link is back, a new stream reports the head it missed.
    proxy
        .blackhole
        .store(false, std::sync::atomic::Ordering::Release);
    let mut again = through_proxy
        .open_events_with(Duration::from_secs(1))
        .unwrap();
    assert_eq!(again.next_event().unwrap(), SyncEvent::Hello { head: 2 });
    assert!(matches!(
        HttpTransport::new(&running.url(), "wrong-token-wrong-token-wrong-token").open_events(),
        Err(app_lite_protocol::TransportError::Unauthorized)
    ));
    assert_eq!(
        through_proxy
            .open_events_with(Duration::from_secs(5))
            .unwrap()
            .next_event()
            .unwrap(),
        SyncEvent::Hello { head: 2 }
    );
}

#[test]
fn a_stream_the_server_ends_on_schedule_says_bye() {
    use app_lite_protocol::SyncEvent;
    let running = start(fast());
    let mut stream = HttpTransport::new(&running.url(), TOKEN)
        .open_events_with(Duration::from_secs(1))
        .unwrap();
    assert_eq!(stream.next_event().unwrap(), SyncEvent::Hello { head: 0 });
    assert_eq!(stream.next_event().unwrap(), SyncEvent::Bye);
    assert!(stream.next_event().is_err(), "closed after bye");
}

/// One raw request; returns the status line, headers and body.
fn raw_request(running: &Running, head: &str, body: &[u8]) -> (String, Vec<u8>) {
    use std::io::Read;
    let mut socket = TcpStream::connect(running.server.local_addr()).unwrap();
    socket.write_all(head.as_bytes()).unwrap();
    socket.write_all(body).unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    (
        String::from_utf8_lossy(&response[..split]).into_owned(),
        response[split + 4..].to_vec(),
    )
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn json_travels_gzip_compressed_both_ways_and_the_limit_counts_decompressed_bytes() {
    use std::io::Read;
    let running = start(fast());
    for op in 'a'..='f' {
        push_note(&running.url(), op);
    }
    let pull = gzip(br#"{"protocol":1,"cursor":0,"limit":100}"#);
    let (head, body) = raw_request(
        &running,
        &format!(
            "POST /v1/pull HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\nContent-Encoding: gzip\r\nAccept-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            pull.len()
        ),
        &pull,
    );
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("Content-Encoding: gzip"), "{head}");
    let mut plain = String::new();
    flate2::read::GzDecoder::new(body.as_slice())
        .read_to_string(&mut plain)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&plain).unwrap()["changes"]
            .as_array()
            .unwrap()
            .len(),
        6
    );

    // 20 MiB of zeros compress to a few KiB: refused by what they expand to.
    let bomb = gzip(&vec![b' '; 20 * 1024 * 1024]);
    assert!(bomb.len() < 64 * 1024);
    let (head, _) = raw_request(
        &running,
        &format!(
            "POST /v1/push HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            bomb.len()
        ),
        &bomb,
    );
    assert!(head.starts_with("HTTP/1.1 413"), "{head}");
}

#[test]
fn a_client_trickling_its_request_head_is_cut_off_at_the_head_deadline() {
    let running = start(HttpOptions {
        socket_timeout: Duration::from_millis(500),
        request_head_timeout: Duration::from_secs(1),
        max_connections: 1,
        ..fast()
    });
    let address = running.server.local_addr();
    let trickler = std::thread::spawn(move || {
        let mut socket = TcpStream::connect(address).unwrap();
        let started = Instant::now();
        // One byte every 300 ms never trips the per-read timeout.
        for byte in b"POST /v1/pull HTTP/1.1\r\nHost: xxxxxxxxxxxxxxxxxxxxxxxx"
            .iter()
            .cycle()
        {
            if socket.write_all(&[*byte]).is_err() || started.elapsed() > Duration::from_secs(10) {
                return started.elapsed();
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        unreachable!()
    });
    let cut_off = trickler.join().unwrap();
    assert!(cut_off < Duration::from_secs(5), "held for {cut_off:?}");
    let transport = HttpTransport::new(&running.url(), TOKEN);
    assert!(
        transport
            .pull(&PullRequest {
                protocol: PROTOCOL_VERSION,
                cursor: 0,
                limit: 1,
            })
            .is_ok()
    );
}

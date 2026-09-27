//! The server terminating TLS itself, with a self-signed certificate the
//! client is told to trust.

use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use app_lite_protocol::{
    PROTOCOL_VERSION, PullRequest, SyncEvent, SyncTransport, TransportError, client::HttpTransport,
};
use app_lite_server::{
    ServerStore,
    http::{HttpOptions, HttpServer, tls_config_from_pem},
};
use tempfile::tempdir;

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

struct Running {
    _root: tempfile::TempDir,
    server: HttpServer,
    certificate_pem: String,
}

impl Running {
    fn url(&self) -> String {
        format!("https://localhost:{}", self.server.local_addr().port())
    }
}

fn start(options: HttpOptions) -> Running {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_pem = certified.cert.pem();
    let tls = tls_config_from_pem(
        certificate_pem.as_bytes(),
        certified.key_pair.serialize_pem().as_bytes(),
    )
    .unwrap();
    let root = tempdir().unwrap();
    let store = Arc::new(ServerStore::open(root.path()).unwrap());
    let server = HttpServer::bind_with(
        "127.0.0.1:0",
        store,
        TOKEN.into(),
        HttpOptions {
            tls: Some(tls),
            ..options
        },
    )
    .unwrap();
    Running {
        _root: root,
        server,
        certificate_pem,
    }
}

fn pull(transport: &HttpTransport) -> Result<usize, TransportError> {
    transport
        .pull(&PullRequest {
            protocol: PROTOCOL_VERSION,
            cursor: 0,
            limit: 10,
        })
        .map(|response| response.changes.len())
}

#[test]
fn https_requests_and_the_event_stream_work_with_a_trusted_self_signed_certificate() {
    let running = start(HttpOptions::default());
    let transport = HttpTransport::with_trusted_certificate(
        &running.url(),
        TOKEN,
        Some(&running.certificate_pem),
    )
    .unwrap();
    assert_eq!(pull(&transport).unwrap(), 0);
    let mut events = transport.open_events().unwrap();
    assert_eq!(events.next_event().unwrap(), SyncEvent::Hello { head: 0 });
}

#[test]
fn an_untrusted_certificate_is_a_permanent_error_not_an_endless_retry() {
    let running = start(HttpOptions::default());
    let untrusting = HttpTransport::new(&running.url(), TOKEN);
    match pull(&untrusting) {
        Err(TransportError::Permanent(reason)) => {
            assert!(reason.contains("not trusted"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        untrusting.open_events(),
        Err(TransportError::Permanent(_))
    ));
}

#[test]
fn plain_http_or_a_stalled_handshake_does_not_disturb_the_tls_listener() {
    let running = start(HttpOptions {
        socket_timeout: Duration::from_millis(500),
        request_head_timeout: Duration::from_millis(500),
        max_connections: 2,
        ..Default::default()
    });
    let plain = HttpTransport::new(
        &format!("http://127.0.0.1:{}", running.server.local_addr().port()),
        TOKEN,
    );
    assert!(pull(&plain).is_err());
    // Connections that never start the handshake fill every slot until the
    // socket timeout frees them.
    let stalled: Vec<TcpStream> = (0..2)
        .map(|_| {
            let mut socket = TcpStream::connect(running.server.local_addr()).unwrap();
            socket.write_all(&[0x16, 0x03, 0x01]).unwrap();
            socket
        })
        .collect();
    std::thread::sleep(Duration::from_millis(1500));
    let started = Instant::now();
    let transport = HttpTransport::with_trusted_certificate(
        &running.url(),
        TOKEN,
        Some(&running.certificate_pem),
    )
    .unwrap();
    assert_eq!(pull(&transport).unwrap(), 0);
    assert!(started.elapsed() < Duration::from_secs(3));
    drop(stalled);
}

//! End-to-end drill over a weak link (run with `--ignored`): HTTPS with a
//! self-signed certificate through a proxy that limits bandwidth, adds
//! latency and cuts every connection after a while, as a phone moving
//! between cells would. Prints timings and counts only.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, sync};
use app_lite_protocol::client::HttpTransport;
use app_lite_protocol::{
    BlobStatus, PullRequest, PullResponse, PushRequest, PushResponse, SyncTransport, TransportError,
};
use app_lite_server::ServerStore;
use app_lite_server::http::{HttpOptions, HttpServer, tls_config_from_pem};
use tempfile::tempdir;

const TOKEN: &str = "0123456789abcdef0123456789abcdef-drill-token";
const BYTES_PER_SECOND: usize = 64 * 1024;
const ONE_WAY_LATENCY: Duration = Duration::from_millis(150);
const CUT_AFTER: Duration = Duration::from_secs(20);

/// Relays with a bandwidth limit and latency per direction, and closes
/// each connection `CUT_AFTER` after it opened.
fn weak_link(upstream: SocketAddr, cuts: Arc<AtomicUsize>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { return };
            let Ok(server) = TcpStream::connect(upstream) else {
                continue;
            };
            let opened = Instant::now();
            for (mut from, mut to) in [
                (client.try_clone().unwrap(), server.try_clone().unwrap()),
                (server, client),
            ] {
                let cuts = Arc::clone(&cuts);
                std::thread::spawn(move || {
                    from.set_read_timeout(Some(Duration::from_millis(50)))
                        .unwrap();
                    let mut buffer = [0u8; 4096];
                    loop {
                        if opened.elapsed() > CUT_AFTER {
                            if from.shutdown(std::net::Shutdown::Both).is_ok() {
                                cuts.fetch_add(1, Ordering::Relaxed);
                            }
                            let _ = to.shutdown(std::net::Shutdown::Both);
                            return;
                        }
                        match from.read(&mut buffer) {
                            Ok(0) => {
                                let _ = to.shutdown(std::net::Shutdown::Write);
                                return;
                            }
                            Ok(count) => {
                                std::thread::sleep(
                                    ONE_WAY_LATENCY / 8
                                        + Duration::from_secs_f64(
                                            count as f64 / BYTES_PER_SECOND as f64,
                                        ),
                                );
                                if to.write_all(&buffer[..count]).is_err() {
                                    return;
                                }
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) => {}
                            Err(_) => return,
                        }
                    }
                });
            }
        }
    });
    address
}

fn text(value: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: value.into(),
            marks: Default::default(),
        }],
    }])
}

fn noise(seed: u32, length: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

/// Logs every request's size, duration and outcome.
struct Logged<'a>(&'a HttpTransport);

fn logged<T>(
    what: String,
    call: impl FnOnce() -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    let started = Instant::now();
    let result = call();
    println!(
        "drill.call {what} {:.1}s {}",
        started.elapsed().as_secs_f64(),
        match &result {
            Ok(_) => "ok".to_owned(),
            Err(error) => format!("{error:?}").chars().take(120).collect(),
        }
    );
    result
}

impl SyncTransport for Logged<'_> {
    fn push(&self, request: &PushRequest) -> Result<PushResponse, TransportError> {
        logged(format!("push {} ops", request.ops.len()), || {
            self.0.push(request)
        })
    }
    fn pull(&self, request: &PullRequest) -> Result<PullResponse, TransportError> {
        logged(format!("pull limit {}", request.limit), || {
            self.0.pull(request)
        })
    }
    fn blob_status(&self, sha256: &str) -> Result<BlobStatus, TransportError> {
        logged("blob_status".into(), || self.0.blob_status(sha256))
    }
    fn put_chunk(
        &self,
        sha256: &str,
        size: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<BlobStatus, TransportError> {
        logged(format!("put_chunk @{offset} +{}", bytes.len()), || {
            self.0.put_chunk(sha256, size, offset, bytes)
        })
    }
    fn read_range(&self, sha256: &str, offset: u64, len: u64) -> Result<Vec<u8>, TransportError> {
        logged(format!("read_range @{offset} +{len}"), || {
            self.0.read_range(sha256, offset, len)
        })
    }
}

/// Sync passes until idle; returns (passes, retryable passes).
fn sync_until_idle(repository: &LibraryRepository, transport: &HttpTransport) -> (usize, usize) {
    let transport = &Logged(transport);
    let mut retried = 0;
    for pass in 1..=400 {
        let started = Instant::now();
        let report = sync::sync_once(repository, transport).expect("no permanent failure");
        println!(
            "drill.pass {pass}: {:.1}s accepted {} pulled {} retryable {} pending {}",
            started.elapsed().as_secs_f64(),
            report.accepted,
            report.pulled,
            report.retryable,
            repository.sync_pending_count().unwrap()
        );
        retried += usize::from(report.retryable > 0);
        if report.retryable == 0
            && report.accepted == 0
            && report.pulled == 0
            && repository.sync_pending_count().unwrap() == 0
        {
            return (pass, retried);
        }
    }
    panic!("did not converge");
}

#[test]
#[ignore = "drill: about two minutes over a throttled link"]
fn a_library_converges_over_a_slow_link_that_keeps_dropping() {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate = certified.cert.pem();
    let server_root = tempdir().unwrap();
    let store = Arc::new(ServerStore::open(server_root.path()).unwrap());
    let server = HttpServer::bind_with(
        "127.0.0.1:0",
        Arc::clone(&store),
        TOKEN.into(),
        HttpOptions {
            tls: Some(
                tls_config_from_pem(
                    certificate.as_bytes(),
                    certified.key_pair.serialize_pem().as_bytes(),
                )
                .unwrap(),
            ),
            ..Default::default()
        },
    )
    .unwrap();
    let cuts = Arc::new(AtomicUsize::new(0));
    let link = weak_link(server.local_addr(), Arc::clone(&cuts));
    let url = format!("https://localhost:{}", link.port());
    let transport =
        HttpTransport::with_trusted_certificate(&url, TOKEN, Some(&certificate)).unwrap();

    let a_root = tempdir().unwrap();
    let a = LibraryRepository::open(a_root.path().join("library.sqlite")).unwrap();
    for index in 0..120 {
        a.create_note(CreateNote {
            title: format!("弱网笔记 {index}"),
            notebook_id: None,
            document: text(&format!("{index} ").repeat(1500)),
        })
        .unwrap();
    }
    let mut pictures = Vec::new();
    for index in 0..3 {
        let bytes = noise(index + 7, 1_500_000);
        let resource = a
            .import_resource(&bytes, &format!("图 {index}.png"), "image/png", "png")
            .unwrap();
        pictures.push((resource, bytes));
    }
    let started = Instant::now();
    let (passes, retried) = sync_until_idle(&a, &transport);
    println!(
        "drill.upload passes {passes} retried {retried} seconds {:.1} cuts {}",
        started.elapsed().as_secs_f64(),
        cuts.load(Ordering::Relaxed)
    );

    let b_root = tempdir().unwrap();
    let b = LibraryRepository::open(b_root.path().join("library.sqlite")).unwrap();
    let started = Instant::now();
    let (passes, retried) = sync_until_idle(&b, &transport);
    println!(
        "drill.download passes {passes} retried {retried} seconds {:.1} cuts {}",
        started.elapsed().as_secs_f64(),
        cuts.load(Ordering::Relaxed)
    );
    let counts = |repository: &LibraryRepository| {
        repository
            .list_notes(Default::default())
            .unwrap()
            .into_iter()
            .map(|note| repository.load_note(&note.id).unwrap().unwrap().body_html)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(counts(&a), counts(&b), "same notes, same bodies");
    for (resource, bytes) in &pictures {
        assert_eq!(
            b.read_resource_bytes(resource).unwrap().as_deref(),
            Some(bytes.as_slice()),
            "attachment arrives byte for byte"
        );
    }
    println!(
        "drill.notes {} attachments {}",
        counts(&b).len(),
        pictures.len()
    );
    assert!(cuts.load(Ordering::Relaxed) > 0, "the link really dropped");
}

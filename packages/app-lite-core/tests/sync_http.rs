//! Two clients against the real HTTP server on localhost
//! (docs/research/sync-client-design-v1.md §3): convergence, conflict copy,
//! an attachment across a server restart, client reopen, bad credentials.

use std::sync::Arc;

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, SaveNote, sync};
use app_lite_protocol::client::HttpTransport;
use app_lite_server::{ServerStore, http::HttpServer};
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

const TOKEN: &str = "sync-http-test-token-0123456789abcdef";

fn text(value: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: value.into(),
            marks: Default::default(),
        }],
    }])
}

/// The server's data directory outlives each process-like `HttpServer`.
struct Server {
    root: TempDir,
    running: Option<HttpServer>,
}

impl Server {
    fn start() -> Self {
        let mut server = Self {
            root: tempdir().unwrap(),
            running: None,
        };
        server.restart();
        server
    }
    fn restart(&mut self) {
        self.running = None;
        let store = Arc::new(ServerStore::open(self.root.path()).unwrap());
        self.running = Some(HttpServer::bind("127.0.0.1:0", store, TOKEN.into()).unwrap());
    }
    fn transport(&self) -> HttpTransport {
        let address = self.running.as_ref().unwrap().local_addr();
        HttpTransport::new(&format!("http://{address}"), TOKEN)
    }
}

fn open(root: &TempDir) -> LibraryRepository {
    LibraryRepository::open(root.path().join("library.sqlite")).unwrap()
}

#[test]
fn two_devices_converge_and_keep_a_conflict_copy_over_http() {
    let server = Server::start();
    let (a_root, b_root) = (tempdir().unwrap(), tempdir().unwrap());
    let a = open(&a_root);
    let note = a
        .create_note(CreateNote {
            title: "会议纪要".into(),
            notebook_id: None,
            document: text("原文"),
        })
        .unwrap();
    sync::sync_once(&a, &server.transport()).unwrap();
    let b = open(&b_root);
    sync::sync_once(&b, &server.transport()).unwrap();
    assert_eq!(
        b.load_note(&note.id).unwrap().unwrap().body_html,
        note.body_html
    );

    for (repo, body) in [(&a, "A 的结论"), (&b, "B 的结论")] {
        let current = repo.load_note(&note.id).unwrap().unwrap();
        repo.save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: current.revision,
            title: "会议纪要".into(),
            document: text(body),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    }
    sync::sync_once(&a, &server.transport()).unwrap();
    sync::sync_once(&b, &server.transport()).unwrap();
    sync::sync_once(&a, &server.transport()).unwrap();
    for repo in [&a, &b] {
        let bodies: Vec<String> = repo
            .list_notes(Default::default())
            .unwrap()
            .into_iter()
            .map(|n| repo.load_note(&n.id).unwrap().unwrap().body_text)
            .collect();
        assert!(bodies.iter().any(|b| b.contains("A 的结论")));
        assert!(
            bodies.iter().any(|b| b.contains("B 的结论")),
            "no edit lost"
        );
        assert_eq!(repo.outbox_count().unwrap(), 0);
    }
}

#[test]
fn an_attachment_survives_a_server_restart_and_a_client_reopen() {
    let mut server = Server::start();
    let (a_root, b_root) = (tempdir().unwrap(), tempdir().unwrap());
    let bytes: Vec<u8> = (0..3_000_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let resource = {
        let a = open(&a_root);
        let resource = a
            .import_resource(&bytes, "扫描件.pdf", "application/pdf", "pdf")
            .unwrap();
        a.create_note(CreateNote {
            title: "带附件".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "扫描件.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
        })
        .unwrap();
        sync::sync_once(&a, &server.transport()).unwrap();
        resource
    };
    // The server process restarts; the client library is reopened.
    server.restart();
    let a = open(&a_root);
    assert_eq!(a.outbox_count().unwrap(), 0);
    let b = open(&b_root);
    sync::sync_once(&b, &server.transport()).unwrap();
    let copied = b
        .read_resource_bytes(&resource)
        .unwrap()
        .expect("attachment arrives");
    assert_eq!(format!("{:x}", Sha256::digest(&copied)), sha);
}

#[test]
fn wrong_credentials_change_nothing_and_are_reported() {
    let server = Server::start();
    let root = tempdir().unwrap();
    let repo = open(&root);
    repo.create_note(CreateNote {
        title: "本地".into(),
        notebook_id: None,
        document: text("x"),
    })
    .unwrap();
    let before = repo.outbox_count().unwrap();
    let address = server.running.as_ref().unwrap().local_addr();
    let wrong = HttpTransport::new(&format!("http://{address}"), "not-the-token");
    assert!(matches!(
        sync::sync_once(&repo, &wrong),
        Err(sync::SyncError::Unauthorized)
    ));
    assert_eq!(
        repo.outbox_count().unwrap(),
        before,
        "local queue untouched"
    );
    let fresh = open(&tempdir().unwrap());
    sync::sync_once(&fresh, &server.transport()).unwrap();
    assert!(
        fresh.list_notes(Default::default()).unwrap().is_empty(),
        "nothing reached the server"
    );
}

#![cfg(feature = "test-support")]

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    AssociateResource, CanonicalDocument, CreateNote, LibraryRepository, ListQuery,
    RepositoryClock, RepositoryIdSource, SaveNote, SearchFilter, SearchIndexTestPhase, SearchQuery,
    SearchTerm,
};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use tempfile::TempDir;

struct Clock(AtomicI64);
impl RepositoryClock for Clock {
    fn now_millis(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}
struct Ids(AtomicU64);
impl RepositoryIdSource for Ids {
    fn next_id(&self) -> Result<String, app_lite_core::LibraryError> {
        Ok(format!("{:032x}", self.0.fetch_add(1, Ordering::SeqCst)))
    }
}
fn repository() -> (TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().unwrap();
    let repo = Arc::new(
        LibraryRepository::open_with_sources(
            profile.path().join("library.sqlite"),
            Arc::new(Clock(AtomicI64::new(10))),
            Arc::new(Ids(AtomicU64::new(1))),
        )
        .unwrap(),
    );
    (profile, repo)
}
fn doc(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}
fn create(repo: &LibraryRepository, title: &str, text: &str) -> app_lite_core::Note {
    repo.create_note(CreateNote {
        title: title.into(),
        notebook_id: None,
        document: doc(text),
    })
    .unwrap()
}

#[test]
fn save_worker_and_reopen_find_chinese_substrings_and_latin_phrases() {
    let (profile, repo) = repository();
    let title = create(&repo, "项目 Alpha", "正文含有北京朝阳和 a quick brown fox");
    let body = create(&repo, "普通标题", "这里也有北京朝阳，另有 quick brown fox");
    assert!(
        repo.search(SearchQuery::parse("北京朝阳"))
            .unwrap()
            .is_empty(),
        "derived index must not be read before worker publication"
    );
    assert_eq!(repo.process_search_jobs().unwrap(), 2);
    let chinese = repo.search(SearchQuery::parse("北京朝阳")).unwrap();
    assert_eq!(chinese.len(), 2);
    assert!(chinese.iter().any(|hit| hit.note.id == title.id));
    assert!(chinese.iter().any(|hit| hit.note.id == body.id));
    assert_eq!(
        repo.search(SearchQuery::parse("北")).unwrap().len(),
        2,
        "one-CJK-character queries use the bounded trigram LIKE fallback"
    );
    assert_eq!(
        repo.search(SearchQuery::parse("北京")).unwrap().len(),
        2,
        "two-CJK-character queries use the bounded trigram LIKE fallback"
    );
    let phrase = repo.search(SearchQuery::parse("\"quick brown\"")).unwrap();
    assert_eq!(phrase.len(), 2);
    drop(repo);
    let reopened = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    assert_eq!(
        reopened.search(SearchQuery::parse("Alpha")).unwrap()[0]
            .note
            .id,
        title.id
    );
}

#[test]
fn v6_profile_bootstraps_fts_without_losing_existing_queue_work() {
    let (profile, repo) = repository();
    let note = create(&repo, "旧配置", "迁移后可搜索");
    let db = profile.path().join("library.sqlite");
    drop(repo);
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute_batch("DROP TABLE search_unicode; DROP TABLE search_trigram; DELETE FROM search_queue; INSERT INTO search_queue(note_id,updated_time,reason) VALUES ('00000000000000000000000000000002', 999, 'pending-before-v7'); PRAGMA user_version=6;").unwrap();
    drop(connection);
    let reopened = LibraryRepository::open(&db).unwrap();
    assert!(
        reopened
            .take_search_jobs(100)
            .unwrap()
            .iter()
            .any(|job| job.note_id == note.id && job.reason == "pending-before-v7")
    );
    reopened.process_search_jobs().unwrap();
    assert_eq!(
        reopened.search(SearchQuery::parse("迁移后可搜索")).unwrap()[0]
            .note
            .id,
        note.id
    );
}

#[test]
fn old_ack_cannot_delete_newer_snapshot_and_rename_replaces_index() {
    let (_profile, repo) = repository();
    let note = create(&repo, "old title", "old body");
    let old = repo.take_search_jobs(100).unwrap();
    let changed = repo
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: "new title".into(),
            document: doc("new body"),
            resource_ids: vec![],
            selected_thumbnail_id: None,
        })
        .unwrap();
    repo.ack_search_jobs(&old).unwrap();
    assert_eq!(
        repo.take_search_jobs(100).unwrap().len(),
        1,
        "full queue identity must preserve a newer snapshot"
    );
    repo.process_search_jobs().unwrap();
    assert!(repo.search(SearchQuery::parse("old")).unwrap().is_empty());
    assert_eq!(
        repo.search(SearchQuery::parse("new title")).unwrap()[0]
            .note
            .id,
        changed.id
    );
}

#[test]
fn active_index_transaction_does_not_block_main_connection_note_load() {
    let (_profile, repo) = repository();
    let first = create(&repo, "index writer", "the index transaction is active");
    let second = create(&repo, "foreground reader", "must remain responsive");
    let (entered_sender, entered) = mpsc::channel();
    let (release_sender, release) = mpsc::channel();
    let release = Arc::new(Mutex::new(release));
    let first_transaction = Arc::new(AtomicBool::new(true));
    repo.set_search_index_transaction_hook_for_test(Arc::new(move |phase| {
        if phase != SearchIndexTestPhase::TransactionStarted
            || !first_transaction.swap(false, Ordering::AcqRel)
        {
            return;
        }
        entered_sender.send(()).expect("signal active transaction");
        release
            .lock()
            .expect("release mutex")
            .recv()
            .expect("release active transaction");
    }));

    let worker_repository = Arc::clone(&repo);
    let worker = std::thread::spawn(move || worker_repository.process_search_jobs());
    entered
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("index transaction is active");

    assert_eq!(
        repo.load_note(&second.id)
            .expect("main connection remains readable during WAL index write")
            .expect("second note exists")
            .id,
        second.id
    );
    release_sender.send(()).expect("release worker");
    assert_eq!(
        worker
            .join()
            .expect("worker thread")
            .expect("worker result"),
        2
    );
    assert!(
        repo.take_search_jobs(100)
            .expect("queue readable")
            .is_empty()
    );
    assert_eq!(
        repo.search(SearchQuery::parse("index writer"))
            .expect("derived projection committed")[0]
            .note
            .id,
        first.id
    );
}

#[test]
fn snapshot_save_waits_off_thread_for_an_active_index_transaction() {
    let (_profile, repo) = repository();
    create(&repo, "index writer", "holds the derived write transaction");
    let editable = create(&repo, "editable", "before snapshot");
    let (entered_sender, entered) = mpsc::channel();
    let (release_sender, release) = mpsc::channel();
    let release = Arc::new(Mutex::new(release));
    let first_transaction = Arc::new(AtomicBool::new(true));
    repo.set_search_index_transaction_hook_for_test(Arc::new(move |phase| {
        if phase != SearchIndexTestPhase::TransactionStarted
            || !first_transaction.swap(false, Ordering::AcqRel)
        {
            return;
        }
        entered_sender.send(()).expect("signal active transaction");
        release
            .lock()
            .expect("release mutex")
            .recv()
            .expect("release index transaction");
    }));
    let index_repository = Arc::clone(&repo);
    let index_worker = std::thread::spawn(move || index_repository.process_search_jobs());
    entered
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("index transaction is active");

    let (saved_sender, saved) = mpsc::channel();
    let (save_started_sender, save_started) = mpsc::channel();
    let save_repository = Arc::clone(&repo);
    let expected_revision = editable.revision;
    let editable_id = editable.id.clone();
    let save_worker = std::thread::spawn(move || {
        save_started_sender
            .send(())
            .expect("signal snapshot worker dispatch");
        let outcome = save_repository.flush_snapshot_note(
            SaveNote {
                id: editable_id,
                expected_revision,
                title: "snapshot after index".into(),
                document: doc("saved through the production snapshot entrypoint"),
                resource_ids: vec![],
                selected_thumbnail_id: None,
            },
            None,
        );
        saved_sender.send(outcome).expect("send snapshot result");
    });
    save_started
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("production snapshot worker dispatched");
    assert!(
        matches!(saved.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the snapshot worker has no synchronous foreground result while the index write is held"
    );
    release_sender.send(()).expect("release index worker");
    assert!(index_worker.join().expect("index worker").is_ok());
    let saved = saved
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("snapshot finishes after bounded index transaction")
        .expect("snapshot result");
    save_worker.join().expect("snapshot worker");
    assert_eq!(saved.title, "snapshot after index");
}

#[test]
fn filters_tag_intersection_trash_and_paging_return_only_projections() {
    let (_profile, repo) = repository();
    let first = create(&repo, "match one", "secret canonical body one");
    let second = create(&repo, "match two", "secret canonical body two");
    let third = create(&repo, "match three", "secret canonical body three");
    let red = repo.create_tag("red").unwrap();
    let blue = repo.create_tag("blue").unwrap();
    repo.set_note_tags(&first.id, &[red.id.clone(), blue.id.clone()])
        .unwrap();
    repo.set_note_tags(&second.id, &[red.id.clone()]).unwrap();
    repo.trash_note(&third.id).unwrap();
    repo.process_search_jobs().unwrap();
    let mut query = SearchQuery::parse("match tag:red tag:blue");
    query.set_page(0, 1).unwrap();
    let observer = repo.observe_next_search_query();
    let hits = repo.search(query).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note.id, first.id);
    let reads = observer.recv().unwrap();
    assert!(
        !reads.iter().any(|field| matches!(
            field.as_str(),
            "notes.body_html" | "notes.body_text" | "notes.merge_state" | "resource_blobs.bytes"
        )),
        "search hits must not hydrate canonical body/blob bytes: {reads:?}"
    );
    let mut trash_query = SearchQuery::parse("match");
    trash_query.filters.push(SearchFilter::Trash(true));
    assert!(
        repo.search(trash_query)
            .unwrap()
            .iter()
            .any(|hit| hit.note.id == third.id)
    );
}

#[test]
fn resource_filters_require_current_attachments_and_report_deterministic_provenance() {
    // This fails if search stops joining current note_resources, ignores a
    // deleted resource, or omits/chooses the wrong attachment provenance.
    let (profile, repo) = repository();
    let pdf = repo
        .import_resource(b"%PDF-1.7", "separate-report.pdf", "application/pdf", "pdf")
        .unwrap();
    let image = repo
        .import_image(b"png", "separate-image.png", "image/png", "png")
        .unwrap();
    let first_report = repo
        .import_resource(b"first", "first-report.pdf", "application/pdf", "pdf")
        .unwrap();
    let second_report = repo
        .import_resource(b"second", "second-report.pdf", "application/pdf", "pdf")
        .unwrap();
    let mixed_image = repo
        .import_image(b"mixed", "mixed-image.png", "image/png", "png")
        .unwrap();
    repo.import_resource(b"orphan", "standalone-report.pdf", "application/pdf", "pdf")
        .unwrap();

    let pdf_note = create(
        &repo,
        "PDF title remains searchable",
        "PDF body remains searchable",
    );
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: pdf_note.id.clone(),
            expected_revision: pdf_note.revision,
            title: pdf_note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "PDF body remains searchable".into(),
                        marks: Default::default(),
                    }],
                },
                Block::Attachment {
                    resource_id: pdf.clone(),
                    filename: "separate-report.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            ]),
            resource_ids: vec![pdf.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    let image_note = create(&repo, "Image note", "image-only body");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: image_note.id.clone(),
            expected_revision: image_note.revision,
            title: image_note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Image {
                    resource_id: image.clone(),
                    alt: "separate image".into(),
                }],
            }]),
            resource_ids: vec![image.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    let mixed_note = create(&repo, "Mixed attachments", "mixed body");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: mixed_note.id.clone(),
            expected_revision: mixed_note.revision,
            title: mixed_note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![
                Block::Attachment {
                    resource_id: first_report.clone(),
                    filename: "first-report.pdf".into(),
                    media_type: "application/pdf".into(),
                },
                Block::Attachment {
                    resource_id: second_report.clone(),
                    filename: "second-report.pdf".into(),
                    media_type: "application/pdf".into(),
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Image {
                        resource_id: mixed_image.clone(),
                        alt: "mixed image".into(),
                    }],
                },
            ]),
            resource_ids: vec![first_report.clone(), second_report, mixed_image],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    repo.process_search_jobs().unwrap();

    let blob_reads = repo.observe_resource_reads();
    let columns = repo.observe_next_search_query();
    let pdf_hits = repo
        .search(SearchQuery::parse("filename:separate-report"))
        .unwrap();
    assert_eq!(pdf_hits.len(), 1);
    assert_eq!(pdf_hits[0].note.id, pdf_note.id);
    assert_eq!(pdf_hits[0].matched_resource, Some(pdf.clone()));
    assert!(
        blob_reads.try_recv().is_err(),
        "filtering must not read blob bytes"
    );
    let reads = columns.recv().unwrap();
    assert!(
        !reads.iter().any(|field| matches!(
            field.as_str(),
            "notes.body_html" | "notes.body_text" | "resource_blobs.bytes"
        )),
        "resource filters must stay on card and attachment metadata: {reads:?}"
    );
    let image_hits = repo.search(SearchQuery::parse("mime:image/png")).unwrap();
    assert_eq!(image_hits.len(), 2);
    assert!(image_hits.iter().any(|hit| {
        hit.note.id == image_note.id && hit.matched_resource == Some(image.clone())
    }));
    let provenance = repo
        .search(SearchQuery::parse("filename:report mime:image/png"))
        .unwrap();
    assert_eq!(provenance.len(), 1);
    assert_eq!(provenance[0].note.id, mixed_note.id);
    assert_eq!(provenance[0].matched_resource, Some(first_report.clone()));
    assert!(
        repo.search(SearchQuery::parse("filename:standalone-report"))
            .unwrap()
            .is_empty(),
        "a standalone resource is not an attachment search result"
    );

    drop(repo);
    let connection = rusqlite::Connection::open(profile.path().join("library.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE note_resources SET is_associated = 0 WHERE note_id = ?1 AND resource_id = ?2",
            rusqlite::params![pdf_note.id.as_str(), pdf.as_str()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE resources SET deleted_time = 1 WHERE id = ?1",
            [image.as_str()],
        )
        .unwrap();
    drop(connection);

    let reopened = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    assert!(
        reopened
            .search(SearchQuery::parse("filename:separate-report"))
            .unwrap()
            .is_empty(),
        "an unassociated historical relation must disappear from attachment search"
    );
    assert!(
        reopened
            .search(SearchQuery::parse("filename:separate-image"))
            .unwrap()
            .is_empty(),
        "a deleted resource must disappear from attachment search"
    );
    let live_attachments = reopened
        .search(SearchQuery::parse("hasattachment:true"))
        .unwrap();
    assert_eq!(live_attachments.len(), 1);
    assert_eq!(live_attachments[0].note.id, mixed_note.id);
    assert_eq!(live_attachments[0].note.attachment_count, 3);
    let no_live_attachments = reopened
        .search(SearchQuery::parse("hasattachment:false"))
        .unwrap();
    assert_eq!(no_live_attachments.len(), 2);
    assert!(no_live_attachments.iter().all(|hit| {
        (hit.note.id == pdf_note.id || hit.note.id == image_note.id)
            && hit.note.attachment_count == 0
    }));
    for query in [
        "PDF title remains searchable",
        "PDF body remains searchable",
    ] {
        let hits = reopened.search(SearchQuery::parse(query)).unwrap();
        assert_eq!(hits.len(), 1, "ordinary search must retain {query:?}");
        assert_eq!(hits[0].note.id, pdf_note.id);
        assert_eq!(hits[0].matched_resource, None);
    }
}

#[test]
fn mixed_filename_filter_and_body_term_keep_both_snippet_sources() {
    // `filename:` proves the attachment relation, while the ordinary term is
    // satisfied only by the note body. Replacing the body snippet here would
    // falsely imply that every term matched attachment-derived text.
    let (_profile, repo) = repository();
    let resource = repo
        .import_resource(b"pdf", "invoice.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = create(&repo, "meeting title", "meeting body summary");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "meeting body summary".into(),
                        marks: Default::default(),
                    }],
                },
                Block::Attachment {
                    resource_id: resource.clone(),
                    filename: "invoice.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            ]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    repo.process_search_jobs().unwrap();

    let hit = repo
        .search(SearchQuery::parse("filename:invoice meeting"))
        .unwrap()
        .pop()
        .expect("mixed query result");
    assert_eq!(hit.note.id, note.id);
    assert_eq!(hit.matched_resource, Some(resource));
    assert_eq!(hit.snippet, "meeting body summary\n匹配附件：invoice.pdf");
    assert_eq!(hit.note.snippet, "meeting body summary\n匹配附件：invoice.pdf");
}

#[test]
fn ordinary_terms_find_live_attachment_filenames_with_provenance() {
    // This fails if ordinary terms only search the note-owned title/body FTS
    // instead of the bounded resource-owned filename projection.
    let (_profile, repo) = repository();
    let resource = repo
        .import_resource(
            b"%PDF-1.7",
            "quarterly-ledger.pdf",
            "application/pdf",
            "pdf",
        )
        .unwrap();
    let note = create(&repo, "Neutral title", "neutral body");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "display-name.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    repo.process_search_jobs().unwrap();

    let blob_reads = repo.observe_resource_reads();
    let columns = repo.observe_next_search_query();
    let hits = repo.search(SearchQuery::parse("ledger")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].note.id, note.id);
    assert_eq!(hits[0].matched_resource, Some(resource));
    assert!(blob_reads.try_recv().is_err());
    let reads = columns.recv().unwrap();
    assert!(
        !reads.iter().any(|field| matches!(
            field.as_str(),
            "notes.body_html" | "notes.body_text" | "resource_blobs.bytes"
        )),
        "ordinary filename search must stay on derived projections: {reads:?}"
    );
}

#[test]
fn ordinary_filename_provenance_uses_fts_word_boundaries() {
    // `LIKE '%cat%'` would incorrectly attribute this `cat` match to the
    // first attachment (`education.pdf`).  Provenance must use the same FTS
    // semantics as the positive ordinary-term predicate.
    let (_profile, repo) = repository();
    let misleading = repo
        .import_resource(b"one", "education.pdf", "application/pdf", "pdf")
        .unwrap();
    let matching = repo
        .import_resource(b"two", "cat-report.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = create(&repo, "neutral", "neutral");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![
                Block::Attachment {
                    resource_id: misleading.clone(),
                    filename: "education.pdf".into(),
                    media_type: "application/pdf".into(),
                },
                Block::Attachment {
                    resource_id: matching.clone(),
                    filename: "cat-report.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            ]),
            resource_ids: vec![misleading, matching.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();

    repo.process_search_jobs().unwrap();

    let hits = repo.search(SearchQuery::parse("cat")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched_resource, Some(matching));
}

#[test]
fn ordinary_filename_provenance_skips_body_terms_and_like_false_positives() {
    // `cat` is satisfied by body text. `education.pdf` must not be presented
    // as provenance merely because the old LIKE-based attribution saw `cat`
    // inside "education".
    let (_profile, repo) = repository();
    let resource = repo
        .import_resource(b"one", "education.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = create(&repo, "neutral", "a cat in the body");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "a cat in the body".into(),
                        marks: Default::default(),
                    }],
                },
                Block::Attachment {
                    resource_id: resource.clone(),
                    filename: "education.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            ]),
            resource_ids: vec![resource],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();

    repo.process_search_jobs().unwrap();

    let hits = repo.search(SearchQuery::parse("cat")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched_resource, None);
}

#[test]
fn ordinary_filename_provenance_uses_first_positive_term_that_hits_a_filename() {
    let (_profile, repo) = repository();
    let resource = repo
        .import_resource(b"one", "quarterly-ledger.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = create(&repo, "neutral", "neutral body only");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title.clone(),
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "quarterly-ledger.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();

    repo.process_search_jobs().unwrap();

    let hits = repo.search(SearchQuery::parse("neutral ledger")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched_resource, Some(resource));
}

#[test]
fn filename_projection_triggers_follow_title_soft_delete_and_hard_delete() {
    let (profile, repo) = repository();
    let resource = repo
        .import_resource(b"one", "initial-ledger.pdf", "application/pdf", "pdf")
        .unwrap();
    let orphan = repo
        .import_resource(b"two", "orphan-ledger.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = create(&repo, "neutral", "neutral");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "display-name.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    assert_eq!(
        repo.search(SearchQuery::parse("initial-ledger"))
            .unwrap()
            .len(),
        1
    );
    drop(repo);

    let path = profile.path().join("library.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE resources SET title = 'renamed-file.pdf' WHERE id = ?1",
            [resource.as_str()],
        )
        .unwrap();
    connection
        .execute("DELETE FROM resources WHERE id = ?1", [orphan.as_str()])
        .unwrap();
    drop(connection);

    let reopened = LibraryRepository::open(&path).unwrap();
    assert!(
        reopened
            .search(SearchQuery::parse("initial-ledger"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        reopened
            .search(SearchQuery::parse("renamed-file"))
            .unwrap()
            .len(),
        1
    );
    drop(reopened);

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE resources SET deleted_time = 1 WHERE id = ?1",
            [resource.as_str()],
        )
        .unwrap();
    let rows: i64 = connection
        .query_row(
            "SELECT count(*) FROM resource_search_rows WHERE resource_id IN (?1, ?2)",
            [resource.as_str(), orphan.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        rows, 1,
        "soft delete retains stable row identity; hard delete removes it"
    );
    let indexed: i64 = connection
        .query_row(
            "SELECT count(*) FROM resource_filename_unicode",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 0, "soft-deleted titles leave both FTS projections");
    drop(connection);

    assert!(
        LibraryRepository::open(&path)
            .unwrap()
            .search(SearchQuery::parse("renamed-file"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn filename_terms_support_short_long_cjk_phrases_and_negation() {
    let (_profile, repo) = repository();
    let resource = repo
        .import_resource(b"one", "北京朝阳季度报告.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = create(&repo, "neutral", "neutral");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "display-name.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();

    for query in ["北", "北京", "北京朝阳", "\"北京朝阳\""] {
        let hits = repo.search(SearchQuery::parse(query)).unwrap();
        assert_eq!(hits.len(), 1, "filename term {query:?}");
        assert_eq!(hits[0].matched_resource, Some(resource.clone()));
    }
    assert!(
        repo.search(SearchQuery::parse("-\"北京朝阳\""))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn quoted_latin_filename_phrase_survives_trash_scope_and_detach() {
    let (_profile, repo) = repository();
    let resource = repo
        .import_resource(
            b"one",
            "quarterly ledger report.pdf",
            "application/pdf",
            "pdf",
        )
        .unwrap();
    let note = create(&repo, "neutral", "neutral");
    let _associated = repo
        .associate_resource(AssociateResource {
            snapshot: SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: note.title.clone(),
                document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                    resource_id: resource.clone(),
                    filename: "display-name.pdf".into(),
                    media_type: "application/pdf".into(),
                }]),
                resource_ids: vec![resource],
                selected_thumbnail_id: None,
            },
        })
        .unwrap();
    let phrase = SearchQuery::parse("\"quarterly ledger\"");
    assert_eq!(repo.search(phrase.clone()).unwrap().len(), 1);

    repo.trash_note(&note.id).unwrap();
    assert!(repo.search(phrase.clone()).unwrap().is_empty());
    assert_eq!(
        repo.search(SearchQuery::parse("trash:true \"quarterly ledger\""))
            .unwrap()
            .len(),
        1
    );

    repo.restore_note(&note.id).unwrap();
    let restored_note = repo.load_note(&note.id).unwrap().unwrap();
    let restored = repo
        .save_note(SaveNote {
            id: restored_note.id,
            expected_revision: restored_note.revision,
            title: restored_note.title,
            document: doc("neutral"),
            resource_ids: Vec::new(),
            selected_thumbnail_id: None,
        })
        .unwrap();
    assert_eq!(restored.id, note.id);
    assert!(repo.search(phrase).unwrap().is_empty());
    assert!(
        repo.search(SearchQuery::parse("ledger"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn restored_resource_repopulates_filename_fts() {
    let (profile, repo) = repository();
    let resource = repo
        .import_resource(b"one", "restore-ledger.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = create(&repo, "neutral", "neutral");
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "display-name.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![resource.clone()],
            selected_thumbnail_id: None,
        },
    })
    .unwrap();
    drop(repo);

    let path = profile.path().join("library.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE resources SET deleted_time = 1 WHERE id = ?1",
            [resource.as_str()],
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM resource_filename_unicode",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    connection
        .execute(
            "UPDATE resources SET deleted_time = 0 WHERE id = ?1",
            [resource.as_str()],
        )
        .unwrap();
    drop(connection);

    let reopened = LibraryRepository::open(&path).unwrap();
    let hits = reopened
        .search(SearchQuery::parse("restore-ledger"))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].matched_resource, Some(resource));
}

#[test]
fn parser_keeps_unknown_operators_as_text_and_supports_escaped_quotes() {
    let parsed = SearchQuery::parse("unknown:value \"a \\\"quoted\\\" phrase\" tag:red tag:blue");
    assert!(
        parsed
            .terms
            .iter()
            .any(|term| matches!(term, SearchTerm::Text(value) if value == "unknown:value"))
    );
    assert!(
        parsed.terms.iter().any(
            |term| matches!(term, SearchTerm::Phrase(value) if value == "a \"quoted\" phrase")
        )
    );
    assert_eq!(
        parsed
            .filters
            .iter()
            .filter(|filter| matches!(filter, SearchFilter::Tag(_)))
            .count(),
        2
    );
    let negative_phrase = SearchQuery::parse("-\"quick brown\"");
    assert!(
        matches!(negative_phrase.terms.as_slice(), [SearchTerm::NegatedPhrase(value)] if value == "quick brown")
    );
    let quoted_notebook = SearchQuery::parse("notebook:\"Work Notes\"");
    assert!(
        matches!(quoted_notebook.filters.as_slice(), [SearchFilter::Notebook(value)] if value == "Work Notes")
    );
    let negative_tag = SearchQuery::parse("-tag:red");
    assert!(
        matches!(negative_tag.filters.as_slice(), [SearchFilter::Not(inner)] if matches!(inner.as_ref(), SearchFilter::Tag(value) if value == "red"))
    );
    for invalid in ["created:abc..123", "updated:100..bad", "created:20..10"] {
        assert!(
            SearchQuery::parse(invalid).filters.is_empty(),
            "{invalid} must remain text"
        );
    }
    for unsupported_negative in [
        "-stack:x",
        "-created:1",
        "-hasattachment:true",
        "-filename:x",
        "-mime:image/png",
        "-trash:true",
    ] {
        let parsed = SearchQuery::parse(unsupported_negative);
        assert!(
            parsed.filters.is_empty(),
            "{unsupported_negative} must safely fall back to text"
        );
        assert!(
            matches!(parsed.terms.as_slice(), [SearchTerm::NegatedText(value)] if value == unsupported_negative.trim_start_matches('-'))
        );
    }
}

#[test]
fn unsupported_negative_filters_never_make_search_fail() {
    let (_profile, repo) = repository();
    create(
        &repo,
        "-stack:x -created:1 -hasattachment:true -filename:x -mime:image/png -trash:true",
        "body",
    );
    repo.process_search_jobs().unwrap();
    for query in [
        "-stack:x",
        "-created:1",
        "-hasattachment:true",
        "-filename:x",
        "-mime:image/png",
        "-trash:true",
    ] {
        assert!(
            repo.search(SearchQuery::parse(query)).is_ok(),
            "{query} must not compile to an unsupported filter"
        );
    }
}

#[test]
fn latin_tokens_and_phrases_do_not_use_substring_matching() {
    let (_profile, repo) = repository();
    let exact = create(&repo, "cat", "quick brown fox");
    create(&repo, "education", "quick brownish fox");
    repo.process_search_jobs().unwrap();
    assert_eq!(
        repo.search(SearchQuery::parse("cat"))
            .unwrap()
            .iter()
            .map(|hit| hit.note.id.clone())
            .collect::<Vec<_>>(),
        vec![exact.id.clone()]
    );
    assert_eq!(
        repo.search(SearchQuery::parse("\"quick brown\""))
            .unwrap()
            .iter()
            .map(|hit| hit.note.id.clone())
            .collect::<Vec<_>>(),
        vec![exact.id]
    );
}

#[test]
fn search_projection_uses_the_same_thumbnail_choice_as_list() {
    let (_profile, repo) = repository();
    let image = repo
        .import_image(b"thumbnail", "cover", "image/png", "png")
        .unwrap();
    let note = create(&repo, "cover match", "body");
    let document = CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Image {
            resource_id: image.clone(),
            alt: "cover".into(),
        }],
    }]);
    repo.associate_resource(AssociateResource {
        snapshot: SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: note.title.clone(),
            document,
            resource_ids: vec![image.clone()],
            selected_thumbnail_id: Some(image.clone()),
        },
    })
    .unwrap();
    repo.process_search_jobs().unwrap();
    assert_eq!(
        repo.search(SearchQuery::parse("cover match")).unwrap()[0]
            .note
            .selected_thumbnail_id,
        repo.list_notes(ListQuery::default())
            .unwrap()
            .into_iter()
            .find(|item| item.id == note.id)
            .unwrap()
            .selected_thumbnail_id
    );
}

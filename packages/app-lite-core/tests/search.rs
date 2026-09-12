#![cfg(feature = "test-support")]

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    AssociateResource, CanonicalDocument, CreateNote, LibraryRepository, ListQuery,
    RepositoryClock, RepositoryIdSource, SaveNote, SearchFilter, SearchQuery, SearchTerm,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
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

use app_lite_core::LibraryRepository;
use rusqlite::Connection;

#[test]
fn recent_queries_deduplicate_trim_and_reopen_without_touching_content_or_outbox() {
    let p = tempfile::tempdir().unwrap();
    let path = p.path().join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    repo.record_search(" 会议246 ").unwrap();
    repo.record_search("会议246").unwrap();
    repo.record_search("   ").unwrap();
    drop(repo);
    let repo = LibraryRepository::open(&path).unwrap();
    let items = repo.list_recent_searches("", 128).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].query, "会议246");
    assert_eq!(items[0].use_count, 2);
    let sql = Connection::open(&path).unwrap();
    assert_eq!(sql.query_row("SELECT count(*) FROM notes", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(sql.query_row("SELECT count(*) FROM sync_outbox", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn recent_prefix_is_literal_case_insensitive_and_utf8_safe() {
    let p = tempfile::tempdir().unwrap();
    let repo = LibraryRepository::open(p.path().join("library.sqlite")).unwrap();
    for query in ["100%合同", "100x合同", "a_b", "axb", "O'Reilly", "会议🙂", "路径\\文件"] {
        repo.record_search(query).unwrap();
    }
    for (prefix, want) in [("100%", "100%合同"), ("a_", "a_b"), ("o'r", "O'Reilly"), ("会议", "会议🙂"), ("路径\\", "路径\\文件")] {
        assert_eq!(repo.list_recent_searches(prefix, 128).unwrap().iter().map(|e| e.query.as_str()).collect::<Vec<_>>(), vec![want]);
    }
    assert!(repo.list_recent_searches("", 0).unwrap().is_empty());
}

#[test]
fn history_retains_newest_128_with_stable_tie_order_and_bounded_reads() {
    let p = tempfile::tempdir().unwrap();
    let path = p.path().join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    let sql = Connection::open(&path).unwrap();
    for n in 0..128 {
        sql.execute("INSERT INTO search_history VALUES(?1,?2,1)", rusqlite::params![format!("old{n:03}"), n]).unwrap();
    }
    repo.record_search("new246").unwrap();
    let entries = repo.list_recent_searches("", usize::MAX).unwrap();
    assert_eq!(entries.len(), 128);
    assert_eq!(entries[0].query, "new246");
    assert!(!entries.iter().any(|e| e.query == "old000"));
    assert_eq!(repo.list_recent_searches("", 3).unwrap().len(), 3);
    repo.clear_search_history().unwrap();
    for query in ["b246", "a246"] {
        sql.execute("INSERT INTO search_history VALUES(?1,7,1)", [query]).unwrap();
    }
    assert_eq!(repo.list_recent_searches("", 128).unwrap().iter().map(|e| e.query.as_str()).collect::<Vec<_>>(), vec!["a246", "b246"]);
}

#[test]
fn deleting_recent_query_also_deletes_only_its_strict_prefixes_then_clear_survives_reopen() {
    let p = tempfile::tempdir().unwrap();
    let path = p.path().join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    for query in ["会", "会议", "会议🙂", "会议🙂更多", "议🙂", "other"] {
        repo.record_search(query).unwrap();
    }
    repo.delete_search_history("会议🙂").unwrap();
    let left = repo.list_recent_searches("", 128).unwrap();
    assert_eq!(left.len(), 3);
    for query in ["会议🙂更多", "议🙂", "other"] { assert!(left.iter().any(|e| e.query == query)); }
    repo.delete_search_history("").unwrap();
    assert_eq!(repo.list_recent_searches("", 128).unwrap().len(), 3);
    repo.clear_search_history().unwrap();
    drop(repo);
    assert!(LibraryRepository::open(&path).unwrap().list_recent_searches("", 128).unwrap().is_empty());
}

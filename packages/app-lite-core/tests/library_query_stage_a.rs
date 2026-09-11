#![cfg(feature = "test-support")]

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryError, LibraryRepository, LibraryRoute, ListQuery,
    ListQueryError, NoteId, RepositoryClock, RepositoryIdSource, SortSpec,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::TryRecvError;
use tempfile::TempDir;

struct IncrementingClock(AtomicI64);

impl RepositoryClock for IncrementingClock {
    fn now_millis(&self) -> i64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

struct IncrementingIds(AtomicU64);

impl RepositoryIdSource for IncrementingIds {
    fn next_id(&self) -> Result<String, app_lite_core::LibraryError> {
        Ok(format!("{:032x}", self.0.fetch_add(1, Ordering::SeqCst)))
    }
}

struct ScriptedIds(Mutex<VecDeque<String>>);

impl RepositoryIdSource for ScriptedIds {
    fn next_id(&self) -> Result<String, LibraryError> {
        self.0
            .lock()
            .expect("scripted ID mutex poisoned")
            .pop_front()
            .ok_or(LibraryError::IdCollisionExhausted)
    }
}

fn repository() -> (TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open_with_sources(
            profile.path().join("library.sqlite"),
            Arc::new(IncrementingClock(AtomicI64::new(10))),
            Arc::new(IncrementingIds(AtomicU64::new(1))),
        )
        .expect("open test library"),
    );
    (profile, repository)
}

fn repository_with_ids(ids: &[&str]) -> (TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open_with_sources(
            profile.path().join("library.sqlite"),
            Arc::new(IncrementingClock(AtomicI64::new(10))),
            Arc::new(ScriptedIds(Mutex::new(
                ids.iter().map(|id| (*id).to_owned()).collect(),
            ))),
        )
        .expect("open scripted-ID test library"),
    );
    (profile, repository)
}

fn document(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

fn create(
    repository: &LibraryRepository,
    title: &str,
    notebook_id: Option<app_lite_core::NotebookId>,
) -> NoteId {
    repository
        .create_note(CreateNote {
            title: title.into(),
            notebook_id,
            document: document("large canonical body must stay unloaded"),
        })
        .expect("create note")
        .id
}

#[test]
fn route_compilation_filters_stack_tag_intersection_trash_and_page_without_body_reads() {
    // Mutation-sensitive: changing the tag HAVING count to a union, omitting
    // the stack subquery, dropping the deletion predicate, removing SQL LIMIT,
    // or hydrating body/blob columns changes at least one assertion below.
    let (_profile, repository) = repository();
    let stack = repository.create_stack("工作组").expect("stack");
    let stacked = repository
        .create_notebook("项目", Some(&stack.id))
        .expect("stacked notebook");
    let other = repository
        .create_notebook("私人", None)
        .expect("other notebook");
    let urgent = repository.create_tag("紧急").expect("urgent tag");
    let reference = repository.create_tag("参考").expect("reference tag");

    let both_in_stack = create(&repository, "Alpha", Some(stacked.id.clone()));
    let only_one_tag = create(&repository, "Bravo", Some(stacked.id.clone()));
    let both_outside_stack = create(&repository, "charlie", Some(other.id.clone()));
    let trashed = create(&repository, "Discarded", Some(stacked.id.clone()));
    repository
        .set_note_tags(&both_in_stack, &[urgent.id.clone(), reference.id.clone()])
        .expect("tag stack note");
    repository
        .set_note_tags(&only_one_tag, &[urgent.id.clone()])
        .expect("tag one-tag note");
    repository
        .set_note_tags(
            &both_outside_stack,
            &[urgent.id.clone(), reference.id.clone()],
        )
        .expect("tag outside-stack note");
    repository.trash_note(&trashed).expect("trash note");

    let stack_ids = repository
        .list_notes(ListQuery::for_route(LibraryRoute::Stack(stack.id.clone())))
        .expect("stack query")
        .into_iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    assert!(stack_ids.contains(&both_in_stack));
    assert!(stack_ids.contains(&only_one_tag));
    assert!(!stack_ids.contains(&both_outside_stack));
    assert!(!stack_ids.contains(&trashed));

    let tag_route = LibraryRoute::tags(vec![reference.id.clone(), urgent.id.clone()])
        .expect("non-empty tag route");
    let tag_ids = repository
        .list_notes(ListQuery::for_route(tag_route))
        .expect("tag intersection query")
        .into_iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    assert_eq!(
        tag_ids.len(),
        2,
        "two-tag filtering must be an intersection"
    );
    assert!(tag_ids.contains(&both_in_stack));
    assert!(tag_ids.contains(&both_outside_stack));
    assert!(!tag_ids.contains(&only_one_tag));

    let trash_ids = repository
        .list_notes(ListQuery::for_route(LibraryRoute::Trash))
        .expect("trash query")
        .into_iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    assert_eq!(trash_ids, vec![trashed.clone()]);

    let observation = repository.observe_next_list_query();
    let page = ListQuery::paged(LibraryRoute::AllNotes, SortSpec::title_ascending(), 1, 1)
        .expect("bounded page");
    let page_items = repository.list_notes(page).expect("paged all-notes query");
    assert_eq!(page_items.len(), 1, "SQL pagination must bound the result");
    let reads = observation.recv().expect("projection read observation");
    assert!(reads.iter().any(|column| column == "notes.id"));
    assert!(reads.iter().all(|column| {
        !matches!(
            column.as_str(),
            "notes.body_html" | "notes.body_text" | "notes.merge_state" | "resource_blobs.bytes"
        )
    }));
}

#[test]
fn title_sort_is_case_insensitive_stable_and_page_size_is_bounded() {
    // Mutation-sensitive: replacing COLLATE NOCASE or omitting the id
    // tie-break makes the deterministic title sequence fail.
    let (_profile, repository) = repository();
    let lower = create(&repository, "alpha", None);
    let upper = create(&repository, "Alpha", None);
    let beta = create(&repository, "beta", None);

    let cards = repository
        .list_notes(
            ListQuery::for_route(LibraryRoute::AllNotes).with_sort(SortSpec::title_ascending()),
        )
        .expect("title query");
    let mut tied = vec![lower.clone(), upper.clone()];
    tied.sort();
    assert_eq!(
        cards.into_iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![tied[0].clone(), tied[1].clone(), beta]
    );
    assert!(
        ListQuery::paged(
            LibraryRoute::AllNotes,
            SortSpec::title_ascending(),
            0,
            ListQuery::MAX_PAGE_SIZE + 1,
        )
        .is_err(),
        "a caller cannot accidentally compile an unbounded page"
    );
    assert_eq!(
        repository.observe_next_list_query().try_recv(),
        Err(TryRecvError::Empty),
        "the rejected page must not issue a SQLite query"
    );
}

#[test]
fn notebook_route_offset_and_page_bounds_are_exact() {
    // Mutation-sensitive: removing the notebook predicate, ignoring OFFSET,
    // or accepting either side of the page contract changes an exact result
    // below rather than merely its length.
    let (_profile, repository) = repository();
    let notebook = repository
        .create_notebook("项目", None)
        .expect("create notebook");
    let alpha = create(&repository, "Alpha", Some(notebook.id.clone()));
    let bravo = create(&repository, "Bravo", Some(notebook.id.clone()));
    let outside = create(&repository, "Charlie", None);
    let discarded = create(&repository, "Discarded", Some(notebook.id.clone()));
    repository
        .trash_note(&discarded)
        .expect("trash notebook note");

    let all_notebook = repository
        .list_notes(
            ListQuery::for_route(LibraryRoute::Notebook(notebook.id.clone()))
                .with_sort(SortSpec::title_ascending()),
        )
        .expect("notebook query");
    assert_eq!(
        all_notebook.iter().map(|card| &card.id).collect::<Vec<_>>(),
        vec![&alpha, &bravo],
        "Notebook must include only its active notes"
    );
    assert!(
        !all_notebook.iter().any(|card| card.id == outside),
        "a note outside the Notebook must not leak through the route predicate"
    );

    let first = repository
        .list_notes(
            ListQuery::paged(
                LibraryRoute::Notebook(notebook.id.clone()),
                SortSpec::title_ascending(),
                0,
                1,
            )
            .expect("first page"),
        )
        .expect("execute first page");
    let second = repository
        .list_notes(
            ListQuery::paged(
                LibraryRoute::Notebook(notebook.id),
                SortSpec::title_ascending(),
                1,
                1,
            )
            .expect("second page"),
        )
        .expect("execute second page");
    assert_eq!(
        first.iter().map(|card| &card.id).collect::<Vec<_>>(),
        vec![&alpha]
    );
    assert_eq!(
        second.iter().map(|card| &card.id).collect::<Vec<_>>(),
        vec![&bravo],
        "OFFSET 1 must select the known second card"
    );
    assert_ne!(first[0].id, second[0].id, "adjacent pages must not overlap");

    assert!(matches!(
        ListQuery::paged(LibraryRoute::AllNotes, SortSpec::title_ascending(), 0, 0),
        Err(ListQueryError::PageLimitOutOfRange { limit: 0, .. })
    ));
    assert!(matches!(
        ListQuery::paged(
            LibraryRoute::AllNotes,
            SortSpec::title_ascending(),
            0,
            ListQuery::MAX_PAGE_SIZE + 1,
        ),
        Err(ListQueryError::PageLimitOutOfRange { .. })
    ));
    #[cfg(target_pointer_width = "64")]
    {
        let overflow_offset = usize::try_from(i64::MAX)
            .expect("i64 maximum fits usize on 64-bit targets")
            .checked_add(1)
            .expect("usize has one value above SQLite signed maximum");
        let overflow = ListQuery::paged(
            LibraryRoute::AllNotes,
            SortSpec::title_ascending(),
            overflow_offset,
            1,
        )
        .expect("usize accepts the host value before SQLite compilation");
        assert!(matches!(
            repository.list_notes(overflow),
            Err(LibraryError::ListQuery(ListQueryError::SqlIntegerOverflow))
        ));
    }
}

#[test]
fn title_tie_break_uses_id_not_insert_row_order() {
    // The lowercase title is inserted first with a *larger* ID. Deleting the
    // SQL `n.id ASC` tie-break therefore exposes SQLite's row/insertion order
    // and makes this assertion RED.
    let (_profile, repository) = repository_with_ids(&[
        // Fresh schema migration allocates one settings-owner ID, and each
        // note create allocates a distinct sync-operation ID after the note.
        "00000000000000000000000000000001",
        "00000000000000000000000000000020",
        "000000000000000000000000000000f1",
        "00000000000000000000000000000010",
        "000000000000000000000000000000f2",
        "00000000000000000000000000000030",
        "000000000000000000000000000000f3",
    ]);
    let inserted_first_high_id = create(&repository, "alpha", None);
    let inserted_second_low_id = create(&repository, "Alpha", None);
    let beta = create(&repository, "beta", None);

    let ids = repository
        .list_notes(
            ListQuery::for_route(LibraryRoute::AllNotes).with_sort(SortSpec::title_ascending()),
        )
        .expect("title query")
        .into_iter()
        .map(|card| card.id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![inserted_second_low_id, inserted_first_high_id, beta],
        "NOCASE-equal titles must use stable opaque-ID ordering, not insertion order"
    );
}

#[test]
fn each_route_starts_with_its_source_backed_updated_or_deleted_sort() {
    // Mutation-sensitive: a single accidental global default sort makes one
    // of these routes flip. The timestamps come from the injected monotonic
    // clock rather than wall-clock ordering.
    let (_profile, repository) = repository();
    let older = create(&repository, "older", None);
    let newer = create(&repository, "newer", None);
    let notebook = repository
        .create_notebook("项目", None)
        .expect("create notebook");
    let notebook_older = create(&repository, "notebook older", Some(notebook.id.clone()));
    let notebook_newer = create(&repository, "notebook newer", Some(notebook.id.clone()));
    let stack = repository.create_stack("工作").expect("create stack");
    let stack_notebook = repository
        .create_notebook("堆栈项目", Some(&stack.id))
        .expect("create stacked notebook");
    let stack_older = create(&repository, "stack older", Some(stack_notebook.id.clone()));
    let stack_newer = create(&repository, "stack newer", Some(stack_notebook.id.clone()));
    let tag = repository.create_tag("紧急").expect("create tag");
    let tag_older = create(&repository, "tag older", None);
    let tag_newer = create(&repository, "tag newer", None);
    repository
        .set_note_tags(&tag_older, &[tag.id.clone()])
        .expect("tag older note");
    repository
        .set_note_tags(&tag_newer, &[tag.id.clone()])
        .expect("tag newer note");
    let all = repository
        .list_notes(ListQuery::for_route(LibraryRoute::AllNotes))
        .expect("all-notes default query");
    assert_eq!(
        all.into_iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![
            tag_newer.clone(),
            tag_older.clone(),
            stack_newer.clone(),
            stack_older.clone(),
            notebook_newer.clone(),
            notebook_older.clone(),
            newer.clone(),
            older.clone()
        ],
        "All Notes defaults to updated descending"
    );

    for (route, expected, label) in [
        (
            LibraryRoute::Notebook(notebook.id),
            vec![notebook_newer, notebook_older],
            "Notebook",
        ),
        (
            LibraryRoute::Stack(stack.id),
            vec![stack_newer, stack_older],
            "Stack",
        ),
        (
            LibraryRoute::tags(vec![tag.id]).expect("tag route"),
            vec![tag_newer.clone(), tag_older.clone()],
            "Tag intersection",
        ),
    ] {
        assert_eq!(
            repository
                .list_notes(ListQuery::for_route(route))
                .expect("route default query")
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            expected,
            "{label} defaults to updated descending independently"
        );
    }

    repository.trash_note(&older).expect("trash older");
    repository.trash_note(&newer).expect("trash newer");
    let trash = repository
        .list_notes(ListQuery::for_route(LibraryRoute::Trash))
        .expect("trash default query");
    assert_eq!(
        trash.into_iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![newer, older],
        "Trash defaults to deleted descending independently of All Notes"
    );
}

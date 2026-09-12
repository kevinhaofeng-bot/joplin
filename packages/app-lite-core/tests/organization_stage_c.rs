//! Task 6 Stage C mutation contracts.
//!
//! These tests intentionally exercise the repository's typed organization
//! boundary rather than synthesizing sidebar labels or direct SQL writes. A
//! failed implementation that replaces an entity, drops a relation, or makes
//! a destructive operation visible before its transaction commits should make
//! one of these durable facts fail after reopen.

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    AssociateResource, CanonicalDocument, CreateNote, LibraryRepository, LibraryRoute, ListQuery,
    Note, NoteId, SaveNote, StackId,
};
use rusqlite::Connection;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(feature = "test-support")]
use std::collections::BTreeSet;

fn repository() -> (tempfile::TempDir, std::path::PathBuf, LibraryRepository) {
    let profile = tempdir().expect("temporary profile");
    let path = profile.path().join("library.sqlite");
    let repository = LibraryRepository::open(&path).expect("open repository");
    (profile, path, repository)
}

fn create_note(
    repository: &LibraryRepository,
    title: &str,
    notebook_id: Option<app_lite_core::NotebookId>,
) -> NoteId {
    repository
        .create_note(CreateNote {
            title: title.to_owned(),
            notebook_id,
            document: CanonicalDocument::default(),
        })
        .expect("create note")
        .id
}

fn image_document(resource_ids: &[app_lite_core::ResourceId]) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: resource_ids
            .iter()
            .enumerate()
            .map(|(position, resource_id)| Inline::Image {
                resource_id: resource_id.clone(),
                alt: format!("resource-{position}"),
            })
            .collect(),
    }])
}

fn associate_images(
    repository: &LibraryRepository,
    note: &Note,
    resource_ids: Vec<app_lite_core::ResourceId>,
) -> Note {
    repository
        .associate_resource(AssociateResource {
            snapshot: SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: note.title.clone(),
                document: image_document(&resource_ids),
                resource_ids: resource_ids.clone(),
                selected_thumbnail_id: resource_ids.first().cloned(),
            },
        })
        .expect("associate resources through the durable snapshot path")
}

#[test]
fn typed_organization_renames_preserve_ids_and_survive_reopen() {
    // Mutation-sensitive: delete/recreate is not a rename. It changes route
    // identity and breaks saved selection/history even when the title looks
    // correct, so assert stable IDs on both sides of a database reopen.
    let (_profile, path, repository) = repository();
    let stack = repository.create_stack("Client").expect("stack");
    let notebook = repository
        .create_notebook("Inbox", Some(&stack.id))
        .expect("notebook");
    let tag = repository.create_tag("Urgent").expect("tag");

    let renamed_stack = repository
        .rename_stack(&stack.id, "客户")
        .expect("rename stack");
    let renamed_notebook = repository
        .rename_notebook(&notebook.id, "收件箱")
        .expect("rename notebook");
    let renamed_tag = repository.rename_tag(&tag.id, "紧急").expect("rename tag");

    assert_eq!(renamed_stack.id, stack.id);
    assert_eq!(renamed_notebook.id, notebook.id);
    assert_eq!(renamed_tag.id, tag.id);
    drop(repository);

    let reopened = LibraryRepository::open(path).expect("reopen repository");
    let index = reopened.list_navigation_index().expect("navigation index");
    assert!(
        index
            .stacks
            .iter()
            .any(|value| value.id == stack.id && value.title == "客户")
    );
    assert!(
        index
            .notebooks
            .iter()
            .any(|value| value.id == notebook.id && value.title == "收件箱")
    );
    assert!(
        index
            .tags
            .iter()
            .any(|value| value.id == tag.id && value.title == "紧急")
    );
}

#[test]
fn moving_and_incrementally_editing_tags_preserves_durable_relationship_order() {
    // Catches an add/remove implementation that materializes a note body or
    // replaces the note row instead of changing only typed relationships.
    let (_profile, path, repository) = repository();
    let destination = repository
        .create_notebook("Destination", None)
        .expect("notebook");
    let first = repository.create_tag("first").expect("first tag");
    let second = repository.create_tag("second").expect("second tag");
    let note = create_note(&repository, "move me", None);

    repository
        .move_selected_note(&note, &destination.id)
        .expect("move selected note");
    repository
        .add_note_tag(&note, &first.id)
        .expect("add first tag");
    repository
        .add_note_tag(&note, &second.id)
        .expect("add second tag");
    repository
        .remove_note_tag(&note, &first.id)
        .expect("remove first tag");

    let moved = repository
        .load_note(&note)
        .expect("load moved")
        .expect("note");
    assert_eq!(moved.notebook_id, destination.id);
    assert_eq!(moved.tag_ids, vec![second.id.clone()]);
    assert_eq!(
        repository
            .list_notes(ListQuery::for_route(
                LibraryRoute::tags(vec![second.id.clone()]).expect("typed tag route"),
            ))
            .expect("tag projection")
            .iter()
            .map(|projection| projection.id.clone())
            .collect::<Vec<_>>(),
        vec![note.clone()]
    );
    drop(repository);

    let reopened = LibraryRepository::open(path).expect("reopen repository");
    assert_eq!(
        reopened
            .load_note(&note)
            .expect("reload moved")
            .expect("note persists")
            .tag_ids,
        vec![second.id]
    );
}

#[test]
fn deleting_notebook_rehomes_notes_and_deleting_tag_removes_its_route_relation() {
    // A soft-deleted active organization must never leave a live note pointing
    // at an unavailable notebook/tag. The next AppModel commit can therefore
    // deterministically fall back without hydrating a wrong session.
    let (_profile, path, repository) = repository();
    let default = repository.default_notebook().expect("default notebook");
    let notebook = repository
        .create_notebook("Retire", None)
        .expect("notebook");
    let tag = repository.create_tag("Retire tag").expect("tag");
    let note = create_note(&repository, "affected", Some(notebook.id.clone()));
    repository.add_note_tag(&note, &tag.id).expect("add tag");

    repository
        .delete_notebook(&notebook.id)
        .expect("delete notebook");
    repository.delete_tag(&tag.id).expect("delete tag");

    let repaired = repository
        .load_note(&note)
        .expect("load note")
        .expect("note");
    assert_eq!(repaired.notebook_id, default.id);
    assert!(repaired.tag_ids.is_empty());
    let index = repository
        .list_navigation_index()
        .expect("navigation index");
    assert!(!index.notebooks.iter().any(|value| value.id == notebook.id));
    assert!(!index.tags.iter().any(|value| value.id == tag.id));
    drop(repository);

    let reopened = LibraryRepository::open(path).expect("reopen repository");
    let persisted = reopened.load_note(&note).expect("reload").expect("note");
    assert_eq!(persisted.notebook_id, default.id);
    assert!(persisted.tag_ids.is_empty());
}

#[test]
fn destroying_stack_detaches_children_without_losing_notes_and_survives_reopen() {
    // Evernote's typed DESTROY_STACK action destroys the container, not its
    // notebook contents. The relation must be cleared durably for both an
    // ordinary and a trashed note: a restart may not resurrect the Stack
    // association or drop either child note.
    let (_profile, path, repository) = repository();
    let stack = repository.create_stack("待解散组").expect("create stack");
    let first_notebook = repository
        .create_notebook("第一本", Some(&stack.id))
        .expect("create first child");
    let second_notebook = repository
        .create_notebook("第二本", Some(&stack.id))
        .expect("create second child");
    let first_note = create_note(&repository, "保留的笔记", Some(first_notebook.id.clone()));
    let trashed_note = create_note(&repository, "废纸篓笔记", Some(second_notebook.id.clone()));
    repository
        .trash_note(&trashed_note)
        .expect("trash child note before disband");

    repository
        .delete_stack(&stack.id)
        .expect("destroy stack container");
    let index = repository
        .list_navigation_index()
        .expect("navigation index");
    assert!(
        index
            .stacks
            .iter()
            .all(|candidate| candidate.id != stack.id)
    );
    for notebook in [&first_notebook, &second_notebook] {
        assert_eq!(
            index
                .notebooks
                .iter()
                .find(|candidate| candidate.id == notebook.id)
                .expect("child notebook remains")
                .stack_id,
            None,
            "disbanding a stack leaves its notebook floating"
        );
    }
    assert_eq!(
        repository
            .load_note(&first_note)
            .expect("load active child")
            .expect("active note remains")
            .notebook_id,
        first_notebook.id
    );
    let trashed = repository
        .load_note(&trashed_note)
        .expect("load trashed child")
        .expect("trashed note remains");
    assert_eq!(trashed.notebook_id, second_notebook.id);
    assert!(trashed.deleted_time.is_some());

    drop(repository);
    let reopened = LibraryRepository::open(path).expect("reopen after disband");
    let index = reopened.list_navigation_index().expect("reopened index");
    assert!(
        index
            .stacks
            .iter()
            .all(|candidate| candidate.id != stack.id)
    );
    for notebook_id in [&first_notebook.id, &second_notebook.id] {
        assert_eq!(
            index
                .notebooks
                .iter()
                .find(|candidate| &candidate.id == notebook_id)
                .expect("child persists after reopen")
                .stack_id,
            None
        );
    }
    assert!(
        reopened
            .load_note(&first_note)
            .expect("reopen active note")
            .is_some()
    );
    assert!(
        reopened
            .load_note(&trashed_note)
            .expect("reopen trashed note")
            .is_some()
    );
}

#[test]
fn failed_stack_destroy_leaves_children_and_notes_unchanged() {
    // Mutation-sensitive failure boundary: validating an unknown typed stack
    // must happen before any child notebook relation is cleared.
    let (_profile, _path, repository) = repository();
    let stack = repository.create_stack("仍应存在").expect("create stack");
    let notebook = repository
        .create_notebook("child", Some(&stack.id))
        .expect("create child");
    let note = create_note(&repository, "不可丢失", Some(notebook.id.clone()));
    let missing = StackId::parse("e".repeat(32)).expect("opaque missing stack ID");

    assert!(repository.delete_stack(&missing).is_err());
    let index = repository
        .list_navigation_index()
        .expect("read unchanged index");
    assert!(
        index
            .stacks
            .iter()
            .any(|candidate| candidate.id == stack.id)
    );
    assert_eq!(
        index
            .notebooks
            .iter()
            .find(|candidate| candidate.id == notebook.id)
            .expect("child remains")
            .stack_id,
        Some(stack.id)
    );
    assert_eq!(
        repository
            .load_note(&note)
            .expect("read unchanged note")
            .expect("note remains")
            .notebook_id,
        notebook.id
    );
}

#[test]
fn permanent_purge_reclaims_only_the_last_resource_occurrence_and_survives_reopen() {
    // A purge must make one occurrence-aware decision while the note delete is
    // still transactional: one resource used by two notes survives the first
    // purge, while a unique resource loses metadata and its content-addressed
    // blob only after the final relationship disappears.
    let (profile, path, repository) = repository();
    let unique = repository
        .import_image(b"unique purge payload", "unique", "image/png", "png")
        .expect("create unique resource");
    let shared = repository
        .import_image(b"shared purge payload", "shared", "image/png", "png")
        .expect("create shared resource");
    let unique_hash = repository
        .resource_metadata(&unique)
        .expect("unique metadata")
        .expect("unique resource")
        .sha256;
    let shared_hash = repository
        .resource_metadata(&shared)
        .expect("shared metadata")
        .expect("shared resource")
        .sha256;
    let unique_note = associate_images(
        &repository,
        &repository
            .create_note(CreateNote {
                title: "unique owner".into(),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create unique owner"),
        vec![unique.clone()],
    );
    let first_shared_note = associate_images(
        &repository,
        &repository
            .create_note(CreateNote {
                title: "first shared owner".into(),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create first shared owner"),
        vec![shared.clone()],
    );
    let second_shared_note = associate_images(
        &repository,
        &repository
            .create_note(CreateNote {
                title: "second shared owner".into(),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create second shared owner"),
        vec![shared.clone()],
    );
    let blob_path = |hash: &app_lite_core::BlobHash| {
        profile
            .path()
            .join("resources")
            .join("blobs")
            .join(hash.as_str())
    };

    repository
        .trash_note(&unique_note.id)
        .expect("trash unique owner");
    repository
        .purge_note(&unique_note.id)
        .expect("purge final unique occurrence");
    assert!(
        repository
            .resource_metadata(&unique)
            .expect("read unique metadata")
            .is_none(),
        "the final unique occurrence must not leave a resources row"
    );
    assert!(
        !blob_path(&unique_hash).exists(),
        "the committed GC queue must reclaim the unreferenced unique blob"
    );
    assert!(
        repository
            .resource_metadata(&shared)
            .expect("read shared metadata")
            .is_some(),
        "a resource referenced by another note must survive the first purge"
    );
    assert!(blob_path(&shared_hash).exists());

    repository
        .trash_note(&first_shared_note.id)
        .expect("trash first shared owner");
    repository
        .purge_note(&first_shared_note.id)
        .expect("purge first shared occurrence");
    assert!(
        repository
            .resource_metadata(&shared)
            .expect("read shared metadata after first purge")
            .is_some(),
        "the remaining note relation is the authority for shared ownership"
    );
    assert!(blob_path(&shared_hash).exists());

    repository
        .trash_note(&second_shared_note.id)
        .expect("trash final shared owner");
    repository
        .purge_note(&second_shared_note.id)
        .expect("purge final shared occurrence");
    assert!(repository.resource_metadata(&shared).unwrap().is_none());
    assert!(!blob_path(&shared_hash).exists());

    drop(repository);
    let reopened = LibraryRepository::open(path).expect("reopen after resource GC");
    assert!(reopened.resource_metadata(&unique).unwrap().is_none());
    assert!(reopened.resource_metadata(&shared).unwrap().is_none());
}

#[test]
fn failed_purge_resource_gc_rolls_back_note_and_resource_visibility_together() {
    // Force the resource metadata delete to abort inside the same SQLite
    // transaction. A partial implementation that tombstones/deletes the note
    // before deciding resource ownership would lose the restoreable note or
    // expose an orphaned resource state here.
    let (_profile, path, repository) = repository();
    let resource = repository
        .import_image(b"transactional purge payload", "atomic", "image/png", "png")
        .expect("create resource");
    let note = associate_images(
        &repository,
        &repository
            .create_note(CreateNote {
                title: "atomic purge owner".into(),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create note"),
        vec![resource.clone()],
    );
    repository.trash_note(&note.id).expect("trash note");
    Connection::open(&path)
        .expect("open fault connection")
        .execute_batch(
            "CREATE TRIGGER abort_resource_gc BEFORE DELETE ON resources
             BEGIN SELECT RAISE(ABORT, 'injected resource GC failure'); END;",
        )
        .expect("install transactional fault trigger");

    assert!(
        repository.purge_note(&note.id).is_err(),
        "an in-transaction resource GC failure must abort permanent delete"
    );
    let retained = repository
        .load_note(&note.id)
        .expect("read retained Trash note")
        .expect("failed purge must not erase note");
    assert!(retained.deleted_time.is_some());
    assert_eq!(retained.resource_ids, vec![resource.clone()]);
    assert!(
        repository
            .resource_metadata(&resource)
            .expect("resource metadata after rollback")
            .is_some(),
        "failed permanent delete cannot expose partially reclaimed metadata"
    );
}

#[cfg(unix)]
#[test]
fn failed_blob_unlink_stays_durable_and_is_recovered_on_the_next_open() {
    // The note/resource transaction has already committed when descriptor
    // cleanup can fail. The durable queue is therefore the recovery authority:
    // it must neither resurrect metadata nor silently forget the physical
    // blob, and a later writable open must finish the same safe unlink.
    let (profile, path, repository) = repository();
    let resource = repository
        .import_image(b"retryable physical GC", "retry", "image/png", "png")
        .expect("create resource");
    let hash = repository
        .resource_metadata(&resource)
        .expect("metadata")
        .expect("resource")
        .sha256;
    let note = associate_images(
        &repository,
        &repository
            .create_note(CreateNote {
                title: "retryable owner".into(),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create note"),
        vec![resource.clone()],
    );
    let blobs = profile.path().join("resources").join("blobs");
    let blob = blobs.join(hash.as_str());
    repository.trash_note(&note.id).expect("trash note");
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o500))
        .expect("temporarily block descriptor unlink");
    repository
        .purge_note(&note.id)
        .expect("database purge commits even when later cleanup is retryable");
    assert!(repository.resource_metadata(&resource).unwrap().is_none());
    assert!(
        blob.exists(),
        "failed unlink leaves only retryable disk garbage"
    );
    assert_eq!(
        Connection::open(&path)
            .expect("inspect durable queue")
            .query_row(
                "SELECT count(*) FROM resource_gc_queue WHERE sha256=?1",
                [hash.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .expect("read queue"),
        1,
        "the failed physical cleanup must remain durable for recovery"
    );
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o700))
        .expect("restore blob directory write permission");
    drop(repository);

    let reopened = LibraryRepository::open(path.clone()).expect("reopen retries durable GC");
    assert!(reopened.resource_metadata(&resource).unwrap().is_none());
    assert!(
        !blob.exists(),
        "successful retry removes the orphaned blob file"
    );
    assert_eq!(
        Connection::open(path)
            .expect("inspect drained queue")
            .query_row(
                "SELECT count(*) FROM resource_gc_queue WHERE sha256=?1",
                [hash.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .expect("read queue"),
        0
    );
}

#[cfg(feature = "test-support")]
#[test]
fn organization_metadata_refresh_reads_no_canonical_body_or_resource_bytes() {
    // AppModel uses this narrow query when a selected note stays selected
    // after a move/tag mutation. An eager replacement with load_note would
    // make the authorizer see body_html/body_text/merge_state even if the
    // caller discarded it before repainting the session.
    let (_profile, _path, repository) = repository();
    let tag = repository.create_tag("metadata").expect("create tag");
    let note = create_note(&repository, "metadata note", None);
    repository
        .add_note_tag(&note, &tag.id)
        .expect("add relation");

    let observer = repository.observe_next_note_organization_state_query();
    let state = repository
        .note_organization_state(&note)
        .expect("metadata query")
        .expect("existing note");
    assert_eq!(state.tag_ids, vec![tag.id]);

    let reads = observer.recv().expect("actual SQLite authorizer reads");
    let forbidden = BTreeSet::from([
        "notes.body_html",
        "notes.body_text",
        "notes.merge_state",
        "resource_blobs.bytes",
    ]);
    assert!(
        !reads
            .iter()
            .any(|column| forbidden.contains(column.as_str())),
        "organization metadata must not hydrate body/blob columns: {reads:?}"
    );
}

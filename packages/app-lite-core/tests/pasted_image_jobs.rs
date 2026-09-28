//! Web images pasted into a note and still being fetched are recorded so a
//! fetch cut short by quitting resumes; the records follow their note.

use std::sync::atomic::AtomicBool;

use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRepository, NoteId, PastedImageJob, ResourceId,
    backup_library, restore_library_backup,
};
use tempfile::tempdir;

fn job(id: &str, note: &NoteId, url: &str) -> PastedImageJob {
    PastedImageJob {
        id: ResourceId::new(id).unwrap(),
        note_id: note.clone(),
        url: url.into(),
        alt: "图".into(),
        link: Some("https://example.com/page".into()),
        attempts: 0,
    }
}

fn note(repository: &LibraryRepository, title: &str) -> NoteId {
    repository
        .create_note(CreateNote {
            title: title.into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>正文</p>").unwrap(),
        })
        .unwrap()
        .id
}

#[test]
fn pasted_image_jobs_are_kept_counted_and_follow_their_note() {
    let root = tempdir().unwrap();
    let profile = root.path().join("library");
    std::fs::create_dir(&profile).unwrap();
    let repository = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let (first, second) = (note(&repository, "一"), note(&repository, "二"));
    let a = job(
        "0123456789abcdef0123456789abcdef",
        &first,
        "https://example.com/a.png",
    );
    let b = job(
        "fedcba9876543210fedcba9876543210",
        &second,
        "https://example.com/b.png",
    );
    repository
        .add_pasted_image_jobs(&first, std::slice::from_ref(&a))
        .unwrap();
    repository
        .add_pasted_image_jobs(&second, std::slice::from_ref(&b))
        .unwrap();
    // Recording the same image again keeps the first record and its count.
    repository.record_pasted_image_attempt(&a.id).unwrap();
    repository
        .add_pasted_image_jobs(&first, std::slice::from_ref(&a))
        .unwrap();
    assert_eq!(
        repository.pasted_image_jobs(Some(&first)).unwrap(),
        vec![PastedImageJob {
            attempts: 1,
            ..a.clone()
        }]
    );
    assert_eq!(repository.pasted_image_jobs(None).unwrap().len(), 2);
    assert_eq!(
        repository.record_pasted_image_attempt(&a.id).unwrap(),
        Some(2)
    );

    // Backed up and restored with the library, attempts and all.
    let backup = root.path().join("backup");
    backup_library(&profile, &backup, &AtomicBool::new(false)).unwrap();
    let restored = root.path().join("restored");
    restore_library_backup(&backup, &restored, &AtomicBool::new(false)).unwrap();
    let copy = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    assert_eq!(
        copy.pasted_image_jobs(None).unwrap(),
        repository.pasted_image_jobs(None).unwrap()
    );
    drop(copy);

    // A note in Trash may come back, so its image is still wanted; a note
    // deleted for good takes its records with it.
    repository.trash_note(&first).unwrap();
    assert_eq!(repository.pasted_image_jobs(Some(&first)).unwrap().len(), 1);
    repository.purge_note(&first).unwrap();
    assert!(
        repository
            .pasted_image_jobs(Some(&first))
            .unwrap()
            .is_empty()
    );
    assert_eq!(repository.record_pasted_image_attempt(&a.id).unwrap(), None);

    repository.remove_pasted_image_job(&b.id).unwrap();
    assert!(repository.pasted_image_jobs(None).unwrap().is_empty());
    drop(repository);
    let reopened = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    assert!(reopened.pasted_image_jobs(None).unwrap().is_empty());
}

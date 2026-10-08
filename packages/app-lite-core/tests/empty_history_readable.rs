//! Regression for the native editor's retained, all-blank paragraph revision.
use app_lite_core::document::{Block, BlockStyle};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRepository, NoteId, SaveNote, export_library_readable,
    export_readable_selection, restore_library_readable, restore_readable_export,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use tempfile::tempdir;

fn blank_paragraphs() -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![
        Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![],
        },
        Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![],
        },
    ])
}

fn history(profile: &Path, id: &NoteId) -> Vec<(i64, String, String, String, i64)> {
    let db = Connection::open(profile.join("library.sqlite")).unwrap();
    db.prepare("SELECT revision,title,body_html,body_text,created_time FROM note_revisions WHERE note_id=?1 ORDER BY revision")
        .unwrap()
        .query_map([id.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))
        .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
}

fn revised_blank_note(repo: &LibraryRepository) -> NoteId {
    let note = repo
        .create_note(CreateNote {
            title: "空白草稿".into(),
            notebook_id: None,
            document: blank_paragraphs(),
        })
        .unwrap();
    repo.save_note(SaveNote {
        id: note.id.clone(),
        expected_revision: note.revision,
        title: "已写正文".into(),
        document: CanonicalDocument::parse_html("<p>正文仍在</p>").unwrap(),
        resource_ids: vec![],
        selected_thumbnail_id: None,
    })
    .unwrap();
    note.id
}

#[test]
fn all_blank_paragraphs_have_the_same_search_projection_as_their_saved_html() {
    let document = blank_paragraphs();
    assert_eq!(document.to_canonical_html().as_str(), "");
    assert_eq!(document.search_text().as_str(), "");
    assert_eq!(
        CanonicalDocument::parse_html("")
            .unwrap()
            .search_text()
            .as_str(),
        ""
    );
    // A real soft break or paragraph style is not an empty placeholder.
    let styled = CanonicalDocument::parse_html("<p data-indent=\"1\"></p><p></p>").unwrap();
    assert!(!styled.to_canonical_html().as_str().is_empty());
    assert_eq!(styled.search_text().as_str(), "\n");
}

#[test]
fn editing_a_blank_draft_does_not_block_either_readable_export() {
    let root = tempdir().unwrap();
    let profile = root.path().join("source");
    fs::create_dir(&profile).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let id = revised_blank_note(&repo);
    export_library_readable(&repo, root.path().join("whole")).unwrap();
    export_readable_selection(&repo, &[id], root.path().join("selected")).unwrap();
}

fn legacy_history_round_trip(whole: bool) {
    let root = tempdir().unwrap();
    let profile = root.path().join("source");
    fs::create_dir(&profile).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let id = revised_blank_note(&repo);
    // Reproduce the old writer's exact field pair in this disposable database.
    Connection::open(profile.join("library.sqlite"))
        .unwrap()
        .execute(
            "UPDATE note_revisions SET body_text='\n' WHERE note_id=?1 AND revision=1",
            [id.as_str()],
        )
        .unwrap();
    let before = history(&profile, &id);
    assert_eq!(before[0].2, "");
    assert_eq!(before[0].3, "\n");
    let bundle = root.path().join("bundle");
    let restored = root.path().join("restored");
    fs::create_dir(&restored).unwrap();
    if whole {
        export_library_readable(&repo, &bundle).unwrap();
        restore_library_readable(&bundle, &restored).unwrap();
    } else {
        export_readable_selection(&repo, &[id.clone()], &bundle).unwrap();
        restore_readable_export(&bundle, &restored).unwrap();
    }
    assert_eq!(
        history(&profile, &id),
        before,
        "export must not rewrite the source"
    );
    assert_eq!(
        history(&restored, &id),
        before,
        "retain every historical field"
    );
    let restored_repo = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    let note = restored_repo.load_note(&id).unwrap().unwrap();
    assert_eq!(note.body_html, "<p>正文仍在</p>");
    assert_eq!(note.body_text, "正文仍在");
}

#[test]
fn whole_readable_preserves_legacy_blank_history_without_rewriting_source() {
    legacy_history_round_trip(true);
}

#[test]
fn selected_readable_preserves_legacy_blank_history_without_rewriting_source() {
    legacy_history_round_trip(false);
}

#[test]
fn meaningful_history_text_mismatch_still_aborts_both_exports() {
    let root = tempdir().unwrap();
    let profile = root.path().join("source");
    fs::create_dir(&profile).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let id = revised_blank_note(&repo);
    Connection::open(profile.join("library.sqlite"))
        .unwrap()
        .execute(
            "UPDATE note_revisions SET body_text='\nBAD' WHERE note_id=?1 AND revision=1",
            [id.as_str()],
        )
        .unwrap();
    let before = history(&profile, &id);
    let whole = root.path().join("whole");
    let selected = root.path().join("selected");
    assert!(export_library_readable(&repo, &whole).is_err());
    assert!(export_readable_selection(&repo, &[id.clone()], &selected).is_err());
    assert!(!whole.exists());
    assert!(!selected.exists());
    assert_eq!(history(&profile, &id), before);
}

#[test]
fn whole_restore_rejects_forged_legacy_text_even_with_a_matching_history_digest() {
    let root = tempdir().unwrap();
    let profile = root.path().join("source");
    fs::create_dir(&profile).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    let id = revised_blank_note(&repo);
    let bundle = root.path().join("whole");
    export_library_readable(&repo, &bundle).unwrap();
    let path = bundle.join(format!("history/{}.json", id.as_str()));
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let manifest_path = bundle.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    for (index, forged) in ["\nBAD", " ", ""].into_iter().enumerate() {
        let mut revisions = original.clone();
        revisions[0]["legacy_empty_body_text"] = forged.into();
        let bytes = serde_json::to_vec(&revisions).unwrap();
        manifest["notes"][0]["history_sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
        fs::write(&path, bytes).unwrap();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let target = root.path().join(format!("rejected-{index}"));
        fs::create_dir(&target).unwrap();
        assert!(restore_library_readable(&bundle, &target).is_err());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    }
}

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRepository, SaveNote, export_readable_library,
    restore_readable_export,
};
use std::fs;
use tempfile::tempdir;

fn text_document(text: &str) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: text.into(),
            marks: Default::default(),
        }],
    }])
}

fn document_with_repeated_attachment(
    prefix: &str,
    resource: app_lite_core::ResourceId,
) -> CanonicalDocument {
    CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![
            Inline::Text {
                text: prefix.into(),
                marks: Default::default(),
            },
            Inline::Image {
                resource_id: resource.clone(),
                alt: "收据".into(),
            },
            Inline::Text {
                text: " 与 ".into(),
                marks: Default::default(),
            },
            Inline::Image {
                resource_id: resource,
                alt: "再次引用".into(),
            },
        ],
    }])
}

#[test]
fn readable_export_restores_multiple_notes_ids_html_and_reused_attachment_bytes() {
    let source_profile = tempdir().unwrap();
    let source_db = source_profile.path().join("library.sqlite");
    let source = LibraryRepository::open(&source_db).unwrap();
    let attachment = source
        .import_resource(
            b"exact attachment bytes",
            "收据 2026.pdf",
            "application/pdf",
            "pdf",
        )
        .unwrap();

    let first = source
        .create_note(CreateNote {
            title: "第一篇带附件".into(),
            notebook_id: None,
            document: text_document("initial"),
        })
        .unwrap();
    let first = source
        .save_note(SaveNote {
            id: first.id,
            expected_revision: first.revision,
            title: "第一篇带附件".into(),
            document: document_with_repeated_attachment("第一篇 ", attachment.clone()),
            resource_ids: vec![attachment.clone(), attachment.clone()],
            selected_thumbnail_id: None,
        })
        .unwrap();
    let second = source
        .create_note(CreateNote {
            title: "第二篇复用附件".into(),
            notebook_id: None,
            document: text_document("第二篇正文"),
        })
        .unwrap();
    let second = source
        .save_note(SaveNote {
            id: second.id,
            expected_revision: second.revision,
            title: "第二篇复用附件".into(),
            document: document_with_repeated_attachment("第二篇 ", attachment.clone()),
            resource_ids: vec![attachment.clone(), attachment.clone()],
            selected_thumbnail_id: None,
        })
        .unwrap();
    let source_attachment = source.resource_metadata(&attachment).unwrap().unwrap();

    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("readable-export");
    let export =
        export_readable_library(&source, &[first.id.clone(), second.id.clone()], &bundle).unwrap();
    assert_eq!(export.note_count, 2);
    assert_eq!(export.resource_count, 1);
    assert!(bundle.join("manifest.json").is_file());
    assert_eq!(
        fs::read_to_string(
            bundle
                .join("notes")
                .join(format!("{}.html", first.id.as_str()))
        )
        .unwrap(),
        first.body_html
    );

    let restore_parent = tempdir().unwrap();
    let restored_profile = restore_parent.path().join("new-empty-profile");
    fs::create_dir(&restored_profile).unwrap();
    drop(source);
    let restored = restore_readable_export(&bundle, &restored_profile).unwrap();
    assert_eq!(restored.note_count, 2);
    assert_eq!(restored.resource_count, 1);

    let reopened = LibraryRepository::open(restored_profile.join("library.sqlite")).unwrap();
    for expected in [&first, &second] {
        let actual = reopened.load_note(&expected.id).unwrap().unwrap();
        assert_eq!(actual.id, expected.id);
        assert_eq!(actual.title, expected.title);
        assert_eq!(actual.body_html, expected.body_html);
        assert_eq!(actual.body_text, expected.body_text);
        assert_eq!(actual.resource_ids, expected.resource_ids);
    }
    let restored_attachment = reopened.resource_metadata(&attachment).unwrap().unwrap();
    assert_eq!(restored_attachment.id, attachment);
    assert_eq!(restored_attachment.sha256, source_attachment.sha256);
    assert_eq!(restored_attachment.size, source_attachment.size);
    assert_eq!(
        reopened
            .read_resource_bytes(&restored_attachment.id)
            .unwrap()
            .unwrap(),
        b"exact attachment bytes"
    );
}

#[test]
fn readable_export_restore_refuses_a_nonempty_profile_before_writing() {
    let parent = tempdir().unwrap();
    let occupied = parent.path().join("occupied-profile");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("existing.txt"), b"must remain").unwrap();

    let bundle = parent.path().join("missing-bundle");
    let error = restore_readable_export(&bundle, &occupied).unwrap_err();
    assert!(error.to_string().contains("empty"));
    assert_eq!(
        fs::read(occupied.join("existing.txt")).unwrap(),
        b"must remain"
    );
}

#[test]
fn readable_export_refuses_nondefault_organization_instead_of_omitting_it() {
    let profile = tempdir().unwrap();
    let repository = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let archive = repository.create_notebook("归档", None).unwrap();
    let note = repository
        .create_note(CreateNote {
            title: "不应静默遗漏".into(),
            notebook_id: Some(archive.id),
            document: text_document("正文"),
        })
        .unwrap();
    let bundle_parent = tempdir().unwrap();
    let destination = bundle_parent.path().join("must-not-exist");
    let error = export_readable_library(&repository, &[note.id], &destination).unwrap_err();
    assert!(error.to_string().contains("default notebook"));
    assert!(
        !destination.exists(),
        "a typed refusal must not publish a partial bundle"
    );
}

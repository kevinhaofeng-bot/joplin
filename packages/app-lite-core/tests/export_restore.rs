use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRepository, SaveNote, export_readable_selection,
    restore_readable_export,
};
use sha2::Digest;
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
    let source_histories = [first.id.clone(), second.id.clone()]
        .into_iter()
        .map(|id| {
            (
                id.clone(),
                source
                    .readable_export_note_state(&id, 10_000, 16 * 1024 * 1024)
                    .unwrap()
                    .unwrap(),
            )
        })
        .collect::<Vec<_>>();

    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("readable-export");
    let export =
        export_readable_selection(&source, &[first.id.clone(), second.id.clone()], &bundle)
            .unwrap();
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
    for (id, expected_history) in source_histories {
        let restored_history = reopened
            .readable_export_note_state(&id, 10_000, 16 * 1024 * 1024)
            .unwrap()
            .unwrap();
        assert_eq!(restored_history, expected_history);
    }
}

#[test]
fn readable_selection_restores_selected_thumbnail() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let image = source
        .import_resource(b"opaque image", "cover.png", "image/png", "png")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "有封面".into(),
            notebook_id: None,
            document: document_with_repeated_attachment("封面", image.clone()),
        })
        .unwrap();
    let source_state = source
        .readable_export_note_state(&note.id, 10_000, 16 * 1024 * 1024)
        .unwrap()
        .unwrap();
    assert_eq!(source_state.selected_thumbnail_id, Some(image.clone()));
    let parent = tempdir().unwrap();
    let bundle = parent.path().join("bundle");
    export_readable_selection(&source, &[note.id.clone()], &bundle).unwrap();
    let restore_parent = tempdir().unwrap();
    let target = restore_parent.path().join("empty");
    fs::create_dir(&target).unwrap();
    restore_readable_export(&bundle, &target).unwrap();
    let reopened = LibraryRepository::open(target.join("library.sqlite")).unwrap();
    assert_eq!(
        reopened
            .readable_export_note_state(&note.id, 10_000, 16 * 1024 * 1024)
            .unwrap()
            .unwrap()
            .selected_thumbnail_id,
        Some(image)
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
    let error = export_readable_selection(&repository, &[note.id], &destination).unwrap_err();
    assert!(error.to_string().contains("default notebook"));
    assert!(
        !destination.exists(),
        "a typed refusal must not publish a partial bundle"
    );
}

#[test]
fn invalid_resource_metadata_leaves_requested_restore_target_empty() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let resource = source
        .import_resource(b"attachment", "report.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "需要附件".into(),
            notebook_id: None,
            document: document_with_repeated_attachment("正文", resource),
        })
        .unwrap();
    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("bundle");
    export_readable_selection(&source, &[note.id], &bundle).unwrap();
    let manifest_path = bundle.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["resources"][0]["mime"] = serde_json::Value::String("not a mime".into());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let parent = tempdir().unwrap();
    let target = parent.path().join("empty-target");
    fs::create_dir(&target).unwrap();
    assert!(restore_readable_export(&bundle, &target).is_err());
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

#[test]
fn restore_rejects_html_that_is_not_already_canonical() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let note = source
        .create_note(CreateNote {
            title: "安全正文".into(),
            notebook_id: None,
            document: text_document("正文"),
        })
        .unwrap();
    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("bundle");
    export_readable_selection(&source, &[note.id.clone()], &bundle).unwrap();
    let html_path = bundle
        .join("notes")
        .join(format!("{}.html", note.id.as_str()));
    let tampered = "<p>正文<script>not retained</script></p>";
    fs::write(&html_path, tampered).unwrap();
    let manifest_path = bundle.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["notes"][0]["body_html_sha256"] =
        serde_json::Value::String(format!("{:x}", sha2::Sha256::digest(tampered.as_bytes())));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let parent = tempdir().unwrap();
    let target = parent.path().join("empty-target");
    fs::create_dir(&target).unwrap();
    assert!(restore_readable_export(&bundle, &target).is_err());
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

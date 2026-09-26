use app_lite_core::document::{
    Alignment, Block, BlockStyle, ImagePresentation, Inline, ListItem, ListKind, Marks,
};
use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryError, LibraryRepository, ReadableExportError,
    ResourceError, SaveNote, export_readable_selection, restore_readable_export,
};
use rusqlite::Connection;
use sha2::Digest;
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::process::Command;
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
                display_width: None,
                link: None,
            },
            Inline::Text {
                text: " 与 ".into(),
                marks: Default::default(),
            },
            Inline::Image {
                resource_id: resource,
                alt: "再次引用".into(),
                display_width: None,
                link: None,
            },
        ],
    }])
}

#[test]
fn readable_export_builds_escaped_browsable_pages_with_relative_resource_links() {
    let profile = tempdir().unwrap();
    let source = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let image = source
        .import_resource(b"image original bytes", "photo.png", "image/png", "png")
        .unwrap();
    let pdf = source
        .import_resource(b"pdf original bytes", "quote.pdf", "application/pdf", "pdf")
        .unwrap();
    let literal = format!("literal src=\":/{}\" remains text", image.as_str());
    let note = source
        .create_note(CreateNote {
            title: "中文 \"<>& 标题".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![
                        Inline::Text {
                            text: "加粗".into(),
                            marks: Marks {
                                bold: true,
                                ..Default::default()
                            },
                        },
                        Inline::Image {
                            resource_id: image.clone(),
                            alt: "行内 \"<&>".into(),
                            display_width: None,
                            link: None,
                        },
                        Inline::Text {
                            text: literal.clone(),
                            marks: Marks::default(),
                        },
                        Inline::Text {
                            text: "网页链接".into(),
                            marks: Marks {
                                link: Some("https://example.com/path?a=1&b=2".into()),
                                ..Default::default()
                            },
                        },
                    ],
                },
                Block::List {
                    kind: ListKind::Checklist,
                    items: vec![ListItem {
                        checked: Some(true),
                        style: BlockStyle::default(),
                        inlines: vec![Inline::Text {
                            text: "完成".into(),
                            marks: Marks::default(),
                        }],
                    }],
                    start: None,
                },
                Block::Image {
                    resource_id: image.clone(),
                    alt: "块图".into(),
                    presentation: ImagePresentation::default(),
                    link: None,
                },
                Block::Attachment {
                    resource_id: pdf.clone(),
                    filename: "文档 \"<&>.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            ]),
        })
        .unwrap();
    let bundle = profile.path().join("bundle");
    export_readable_selection(&source, &[note.id.clone()], &bundle).unwrap();

    let index = fs::read_to_string(bundle.join("index.html")).unwrap();
    let page =
        fs::read_to_string(bundle.join(format!("readable/{}.html", note.id.as_str()))).unwrap();
    assert!(index.contains("中文 &quot;&lt;&gt;&amp; 标题"));
    assert!(index.contains(&format!("href=\"readable/{}.html\"", note.id.as_str())));
    assert!(page.contains("<meta charset=\"utf-8\">"));
    assert!(page.contains("<title>中文 &quot;&lt;&gt;&amp; 标题</title>"));
    assert!(page.contains("<strong>加粗</strong>"));
    assert!(page.contains("<ul data-type=\"checklist\"><li data-checked=\"true\">完成</li></ul>"));
    assert!(page.contains("alt=\"行内 &quot;&lt;&amp;&gt;\""));
    assert!(page.contains(&literal));
    assert!(page.contains("href=\"https://example.com/path?a=1&amp;b=2\""));
    assert!(page.contains("文档 \"&lt;&amp;&gt;.pdf"));

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
    let resource_path = |id: &str| {
        manifest["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|resource| resource["id"] == id)
            .unwrap()["relative_path"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let image_path = resource_path(image.as_str());
    let pdf_path = resource_path(pdf.as_str());
    assert_eq!(page.matches(&format!("src=\"../{image_path}\"")).count(), 2);
    assert!(page.contains(&format!("href=\"../{pdf_path}\"")));
    let inline_image = page
        .split_once("<img src=")
        .unwrap()
        .1
        .split_once('>')
        .unwrap()
        .0;
    assert!(!inline_image.contains("data-joplin-lite-block-image"));
    assert!(page.contains("<img data-joplin-lite-block-image=\"true\""));
    assert!(page.contains("img[data-joplin-lite-block-image=\"true\"]{display:block}"));
    assert!(!page.contains("img{display:block}"));
    for (path, expected) in [
        (&image_path, b"image original bytes".as_slice()),
        (&pdf_path, b"pdf original bytes".as_slice()),
    ] {
        let relative = std::path::Path::new("readable").join(format!("../{path}"));
        let actual = fs::read(bundle.join(relative)).unwrap();
        assert_eq!(actual, expected);
        let manifest_hash = manifest["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|resource| resource["relative_path"] == path.as_str())
            .unwrap()["sha256"]
            .as_str()
            .unwrap();
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(&actual)),
            manifest_hash
        );
    }
    assert_eq!(
        fs::read_to_string(bundle.join(format!("notes/{}.html", note.id.as_str()))).unwrap(),
        note.body_html
    );

    // Browser projections are disposable: recovery reads only canonical files.
    fs::write(
        bundle.join(format!("readable/{}.html", note.id.as_str())),
        "broken",
    )
    .unwrap();
    fs::write(bundle.join("index.html"), "broken").unwrap();
    let target = profile.path().join("empty");
    fs::create_dir(&target).unwrap();
    restore_readable_export(&bundle, &target).unwrap();
    let restored = LibraryRepository::open(target.join("library.sqlite")).unwrap();
    assert_eq!(
        restored.load_note(&note.id).unwrap().unwrap().body_html,
        note.body_html
    );
}

#[test]
fn readable_page_applies_canonical_alignment_indent_and_image_display_width() {
    let profile = tempdir().unwrap();
    let source = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let image = source
        .import_resource(b"image bytes", "image.png", "image/png", "png")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "Layout".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle {
                        alignment: Alignment::Center,
                        indent: 2,
                    },
                    inlines: vec![Inline::Text {
                        text: "Centered".into(),
                        marks: Marks::default(),
                    }],
                },
                Block::List {
                    kind: ListKind::Ordered,
                    items: vec![ListItem {
                        checked: None,
                        style: BlockStyle {
                            alignment: Alignment::Right,
                            indent: 8,
                        },
                        inlines: vec![Inline::Text {
                            text: "Right".into(),
                            marks: Marks::default(),
                        }],
                    }],
                    start: None,
                },
                Block::Image {
                    resource_id: image,
                    alt: "Sized".into(),
                    presentation: ImagePresentation {
                        natural_size: Some((1000, 500)),
                        display_width: Some(320),
                    },
                    link: None,
                },
            ]),
        })
        .unwrap();
    let bundle = profile.path().join("bundle");
    export_readable_selection(&source, &[note.id.clone()], &bundle).unwrap();
    let page =
        fs::read_to_string(bundle.join(format!("readable/{}.html", note.id.as_str()))).unwrap();
    assert!(page.contains("[data-align=center]{text-align:center}"));
    assert!(page.contains("[data-align=right]{text-align:right}"));
    assert!(page.contains("[data-indent=\"2\"]{margin-left:2em}"));
    assert!(page.contains("[data-indent=\"8\"]{margin-left:8em}"));
    assert!(page.contains("<p data-align=\"center\" data-indent=\"2\">Centered</p>"));
    assert!(page.contains("<li data-align=\"right\" data-indent=\"8\">Right</li>"));
    assert!(page.contains(
        "data-joplin-lite-display-width=\"320\" style=\"width:320px;max-width:100%;height:auto\""
    ));
}

#[test]
fn readable_export_restores_durable_notebook_and_resource_metadata() {
    let source_profile = tempdir().unwrap();
    let database = source_profile.path().join("library.sqlite");
    let source = LibraryRepository::open(&database).unwrap();
    let original_default = source.default_notebook().unwrap();
    let renamed = source
        .rename_notebook(&original_default.id, "Renamed default")
        .unwrap();
    assert!(renamed.revision > 1);
    let resource = source
        .import_resource(
            b"durable bytes",
            "source.bin",
            "application/octet-stream",
            "bin",
        )
        .unwrap();
    let db = Connection::open(&database).unwrap();
    db.execute(
        "UPDATE notebooks SET created_time=111, updated_time=222 WHERE id=?1",
        [renamed.id.as_str()],
    )
    .unwrap();
    db.execute(
        "UPDATE resources SET created_time=333, updated_time=444 WHERE id=?1",
        [resource.as_str()],
    )
    .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "Metadata".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "source.bin".into(),
                media_type: "application/octet-stream".into(),
            }]),
        })
        .unwrap();
    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("bundle");
    export_readable_selection(&source, &[note.id], &bundle).unwrap();

    let target_parent = tempdir().unwrap();
    let target = target_parent.path().join("empty");
    fs::create_dir(&target).unwrap();
    restore_readable_export(&bundle, &target).unwrap();
    let reopened = Connection::open(target.join("library.sqlite")).unwrap();
    let notebook: (String, i64, i64, i64) = reopened
        .query_row(
            "SELECT title,revision,created_time,updated_time FROM notebooks WHERE is_default=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        notebook,
        ("Renamed default".into(), renamed.revision, 111, 222)
    );
    let resource_metadata: (i64, i64) = reopened
        .query_row(
            "SELECT created_time,updated_time FROM resources WHERE id=?1",
            [resource.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(resource_metadata, (333, 444));
}

#[test]
fn readable_export_rejects_a_blob_physically_larger_than_captured_metadata() {
    let profile = tempdir().unwrap();
    let source = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
    let resource = source
        .import_resource(b"fixed", "fixed.bin", "application/octet-stream", "bin")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "oversized blob".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource.clone(),
                filename: "fixed.bin".into(),
                media_type: "application/octet-stream".into(),
            }]),
        })
        .unwrap();
    let metadata = source.resource_metadata(&resource).unwrap().unwrap();
    let path = profile
        .path()
        .join("resources/blobs")
        .join(metadata.sha256.as_str());
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b"more")
        .unwrap();
    let bundle = profile.path().join("must-not-publish");
    let error = export_readable_selection(&source, &[note.id], &bundle).unwrap_err();
    assert!(matches!(
        error,
        ReadableExportError::Repository(LibraryError::Resource(ResourceError::SizeLimitExceeded))
    ));
    assert!(!bundle.exists());
}

#[test]
fn readable_restore_rejects_old_incomplete_bundle_version() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let resource = source
        .import_resource(b"old bytes", "old.bin", "application/octet-stream", "bin")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "Version".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource,
                filename: "old.bin".into(),
                media_type: "application/octet-stream".into(),
            }]),
        })
        .unwrap();
    let parent = tempdir().unwrap();
    let bundle = parent.path().join("bundle");
    export_readable_selection(&source, &[note.id], &bundle).unwrap();
    let path = bundle.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["version"] = 1.into();
    let notebook = manifest["default_notebook"].as_object_mut().unwrap();
    notebook.remove("revision");
    notebook.remove("created_time");
    notebook.remove("updated_time");
    for resource in manifest["resources"].as_array_mut().unwrap() {
        let resource = resource.as_object_mut().unwrap();
        resource.remove("created_time");
        resource.remove("updated_time");
    }
    fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let target = parent.path().join("target");
    fs::create_dir(&target).unwrap();
    let error = restore_readable_export(&bundle, &target).unwrap_err();
    assert!(format!("{error}").contains("incompatible bundle version"));
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

#[test]
fn readable_export_rejects_default_notebook_inside_a_stack() {
    let profile = tempdir().unwrap();
    let database = profile.path().join("library.sqlite");
    let source = LibraryRepository::open(&database).unwrap();
    let default = source.default_notebook().unwrap();
    let stack = source.create_stack("Grouped").unwrap();
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE notebooks SET stack_id=?1 WHERE id=?2",
            [stack.id.as_str(), default.id.as_str()],
        )
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "Nested".into(),
            notebook_id: None,
            document: text_document("No flattening"),
        })
        .unwrap();
    let bundle = profile.path().join("must-not-publish");
    assert!(export_readable_selection(&source, &[note.id], &bundle).is_err());
    assert!(!bundle.exists());
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
fn readable_export_restores_resources_used_only_by_retained_history() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let historical_image = source
        .import_resource(b"historical image bytes", "before.png", "image/png", "png")
        .unwrap();
    let current_attachment = source
        .import_resource(b"current PDF bytes", "after.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "修订附件".into(),
            notebook_id: None,
            document: document_with_repeated_attachment("旧版", historical_image.clone()),
        })
        .unwrap();
    let first_revision = note.body_html.clone();
    let note = source
        .save_note(SaveNote {
            id: note.id,
            expected_revision: note.revision,
            title: "修订附件".into(),
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: current_attachment.clone(),
                filename: "after.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![current_attachment.clone()],
            selected_thumbnail_id: None,
        })
        .unwrap();

    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("bundle");
    let report = export_readable_selection(&source, &[note.id.clone()], &bundle).unwrap();
    assert_eq!(report.resource_count, 2);

    let target_parent = tempdir().unwrap();
    let target = target_parent.path().join("restored");
    fs::create_dir(&target).unwrap();
    restore_readable_export(&bundle, &target).unwrap();
    let reopened = LibraryRepository::open(target.join("library.sqlite")).unwrap();
    let restored_history = reopened
        .readable_export_note_state(&note.id, 10_000, 16 * 1024 * 1024)
        .unwrap()
        .unwrap();
    assert_eq!(restored_history.revisions[0].body_html, first_revision);
    for (id, expected_bytes) in [
        (historical_image, b"historical image bytes".as_slice()),
        (current_attachment, b"current PDF bytes".as_slice()),
    ] {
        assert_eq!(
            reopened.read_resource_bytes(&id).unwrap().unwrap(),
            expected_bytes
        );
    }
}

#[test]
fn readable_export_refuses_missing_or_corrupt_history_only_resource_bytes() {
    for corrupt in [false, true] {
        let source_profile = tempdir().unwrap();
        let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
        let historical_image = source
            .import_resource(b"historical image bytes", "before.png", "image/png", "png")
            .unwrap();
        let current_attachment = source
            .import_resource(b"current PDF bytes", "after.pdf", "application/pdf", "pdf")
            .unwrap();
        let note = source
            .create_note(CreateNote {
                title: "历史资源损坏".into(),
                notebook_id: None,
                document: document_with_repeated_attachment("旧版", historical_image.clone()),
            })
            .unwrap();
        source
            .save_note(SaveNote {
                id: note.id.clone(),
                expected_revision: note.revision,
                title: "历史资源损坏".into(),
                document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                    resource_id: current_attachment.clone(),
                    filename: "after.pdf".into(),
                    media_type: "application/pdf".into(),
                }]),
                resource_ids: vec![current_attachment],
                selected_thumbnail_id: None,
            })
            .unwrap();
        let metadata = source
            .resource_metadata(&historical_image)
            .unwrap()
            .unwrap();
        let blob = source_profile
            .path()
            .join("resources/blobs")
            .join(metadata.sha256.as_str());
        if corrupt {
            fs::write(blob, b"corrupt historical image bytes").unwrap();
        } else {
            fs::remove_file(blob).unwrap();
        }

        let bundle_parent = tempdir().unwrap();
        let bundle = bundle_parent.path().join("must-not-publish");
        assert!(export_readable_selection(&source, &[note.id], &bundle).is_err());
        assert!(!bundle.exists());
    }
}

#[test]
fn restore_rejects_bundle_missing_a_resource_used_by_history() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let historical_image = source
        .import_resource(b"historical image bytes", "before.png", "image/png", "png")
        .unwrap();
    let current_attachment = source
        .import_resource(b"current PDF bytes", "after.pdf", "application/pdf", "pdf")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "历史资源引用".into(),
            notebook_id: None,
            document: document_with_repeated_attachment("旧版", historical_image.clone()),
        })
        .unwrap();
    source
        .save_note(SaveNote {
            id: note.id.clone(),
            expected_revision: note.revision,
            title: "历史资源引用".into(),
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: current_attachment.clone(),
                filename: "after.pdf".into(),
                media_type: "application/pdf".into(),
            }]),
            resource_ids: vec![current_attachment],
            selected_thumbnail_id: None,
        })
        .unwrap();

    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("bundle");
    export_readable_selection(&source, &[note.id], &bundle).unwrap();
    let manifest_path = bundle.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let resources = manifest["resources"].as_array_mut().unwrap();
    let historical = resources
        .iter()
        .find(|resource| resource["id"] == historical_image.as_str())
        .cloned()
        .expect("the exported bundle includes the historical image resource");
    let relative_path = historical["relative_path"].as_str().unwrap();
    fs::remove_file(bundle.join(relative_path)).unwrap();
    resources.retain(|resource| resource["id"] != historical_image.as_str());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let target_parent = tempdir().unwrap();
    let target = target_parent.path().join("empty-target");
    fs::create_dir(&target).unwrap();
    assert!(restore_readable_export(&bundle, &target).is_err());
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

#[test]
fn readable_export_refuses_current_html_over_restore_limit_before_publishing() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let text = "x".repeat(4 * 1024 * 1024 - 1);
    let note = source
        .create_note(CreateNote {
            title: "超限正文".into(),
            notebook_id: None,
            document: text_document(&text),
        })
        .unwrap();
    assert!(note.body_html.len() > 4 * 1024 * 1024);
    assert!(note.body_text.len() <= 4 * 1024 * 1024);

    let bundle_parent = tempdir().unwrap();
    let bundle = bundle_parent.path().join("must-not-publish");
    assert!(export_readable_selection(&source, &[note.id], &bundle).is_err());
    assert!(!bundle.exists());
}

#[test]
fn restore_many_resources_under_low_file_descriptor_limit() {
    const CHILD: &str = "APP_LITE_RESTORE_LOW_FD_CHILD";
    if let Ok(parent) = std::env::var(CHILD) {
        let parent = std::path::Path::new(&parent);
        let mut limit = libc::rlimit {
            rlim_cur: 48,
            rlim_max: 48,
        };
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        let target = parent.join("restored");
        fs::create_dir(&target).unwrap();
        let result = restore_readable_export(parent.join("bundle"), &target);
        assert!(
            result.is_ok(),
            "restore failed under 48 descriptors: {result:?}"
        );
        assert_eq!(result.unwrap().resource_count, 64);
        return;
    }

    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let mut blocks = Vec::new();
    for index in 0..64 {
        let resource = source
            .import_resource(
                format!("resource {index}").as_bytes(),
                &format!("attachment-{index}.bin"),
                "application/octet-stream",
                "bin",
            )
            .unwrap();
        blocks.push(Block::Attachment {
            resource_id: resource,
            filename: format!("attachment-{index}.bin"),
            media_type: "application/octet-stream".into(),
        });
    }
    let note = source
        .create_note(CreateNote {
            title: "many resources".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(blocks),
        })
        .unwrap();
    let parent = tempdir().unwrap();
    export_readable_selection(&source, &[note.id], parent.path().join("bundle")).unwrap();
    drop(source);
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("restore_many_resources_under_low_file_descriptor_limit")
        .arg("--nocapture")
        .env(CHILD, parent.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "low-limit child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn restore_rejects_symlinks_at_each_bundle_boundary() {
    let source_profile = tempdir().unwrap();
    let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
    let resource = source
        .import_resource(b"attachment", "file.bin", "application/octet-stream", "bin")
        .unwrap();
    let note = source
        .create_note(CreateNote {
            title: "symlink test".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                resource_id: resource,
                filename: "file.bin".into(),
                media_type: "application/octet-stream".into(),
            }]),
        })
        .unwrap();
    let parent = tempdir().unwrap();
    let bundle = parent.path().join("bundle");
    export_readable_selection(&source, &[note.id], &bundle).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
    let html = manifest["notes"][0]["html_path"].as_str().unwrap();
    let resource = manifest["resources"][0]["relative_path"].as_str().unwrap();

    let alias = parent.path().join("bundle-alias");
    symlink(&bundle, &alias).unwrap();
    let root_target = parent.path().join("root-target");
    fs::create_dir(&root_target).unwrap();
    assert!(restore_readable_export(&alias, &root_target).is_err());
    let alias_with_separator = std::path::PathBuf::from(format!("{}/", alias.display()));
    assert!(restore_readable_export(&alias_with_separator, &root_target).is_err());
    let alias_with_dot = std::path::PathBuf::from(format!("{}/.", alias.display()));
    assert!(restore_readable_export(&alias_with_dot, &root_target).is_err());
    assert!(fs::read_dir(&root_target).unwrap().next().is_none());

    for (index, entry) in ["notes", "resources", "manifest.json", html, resource]
        .into_iter()
        .enumerate()
    {
        let original = bundle.join(entry);
        let held = original.with_extension(format!("held-{index}"));
        fs::rename(&original, &held).unwrap();
        symlink(&held, &original).unwrap();
        let target = parent.path().join(format!("target-{index}"));
        fs::create_dir(&target).unwrap();
        assert!(
            restore_readable_export(&bundle, &target).is_err(),
            "accepted symlink at {entry}"
        );
        assert!(fs::read_dir(&target).unwrap().next().is_none());
        fs::remove_file(&original).unwrap();
        fs::rename(&held, &original).unwrap();
    }
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

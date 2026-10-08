use app_lite_core::{CanonicalDocument, convert_enml, convert_jex_note_body};
use std::collections::BTreeMap;

const NOTE: &str = "111111111111111111111111111111aa";
const HTML: &str = "<p>前<a href=\"https://example.test/note\" title=\"中文 &amp; &quot;说明&quot;\"><strong>链接</strong></a>后</p>";

#[test]
fn link_title_survives_canonical_and_clipboard_roundtrips_without_becoming_body_text() {
    for doc in [
        CanonicalDocument::parse_html(HTML).unwrap(),
        CanonicalDocument::parse_pasted_html(HTML).unwrap().document,
    ] {
        assert_eq!(doc.to_canonical_html().as_str(), HTML);
        assert_eq!(doc.search_text().as_str(), "前链接后");
        assert_eq!(CanonicalDocument::parse_html(HTML).unwrap(), doc);
    }
}

#[test]
fn jex_markdown_and_html_link_titles_are_kept_not_reported_as_lossy() {
    for (markup, body) in [
        (
            1,
            "前[**链接**](https://example.test/note '中文 & \"说明\"')后",
        ),
        (2, HTML),
    ] {
        let converted = convert_jex_note_body(NOTE, "title.md", markup, body, &BTreeMap::new())
            .unwrap_or_else(|error| panic!("markup={markup}: {error:?}"));
        assert_eq!(converted.canonical_html, HTML);
        assert_eq!(converted.search_text, "前链接后");
        assert_eq!(
            CanonicalDocument::parse_html(&converted.canonical_html).unwrap(),
            converted.document
        );
    }
}

#[test]
fn title_does_not_make_an_unsafe_url_safe_or_silently_truncate_import_metadata() {
    for body in [
        "[x](javascript:alert(1) \"title\")".to_owned(),
        format!("[x](https://example.test \"{}\")", "x".repeat(4097)),
    ] {
        assert!(convert_jex_note_body(NOTE, "unsafe.md", 1, &body, &BTreeMap::new()).is_err());
    }
    for body in [
        "<p><a href=\"javascript:alert(1)\" title=\"tip\">x</a></p>".to_owned(),
        format!(
            "<p><a href=\"https://example.test\" title=\"{}\">x</a></p>",
            "x".repeat(4097)
        ),
    ] {
        assert!(convert_jex_note_body(NOTE, "unsafe.html", 2, &body, &BTreeMap::new()).is_err());
    }
}

#[test]
fn enml_link_title_is_preserved_as_attribute_not_visible_text() {
    let converted = convert_enml(&format!("<en-note>{HTML}</en-note>"), &BTreeMap::new()).unwrap();
    assert_eq!(converted.html.as_str(), HTML);
    assert_eq!(converted.search_text.as_str(), "前链接后");
}

#[test]
fn adjacent_same_url_different_titles_are_not_merged() {
    let html = "<p><a href=\"https://example.test\" title=\"一\">甲</a><a href=\"https://example.test\" title=\"二\">乙</a>丙</p>";
    assert_eq!(
        CanonicalDocument::parse_html(html)
            .unwrap()
            .to_canonical_html()
            .as_str(),
        html
    );
}

#[test]
fn titled_resource_and_fragment_links_are_not_silently_flattened() {
    for body in [
        "[x](#anchor \"title\")",
        "[x](:/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \"title\")",
    ] {
        assert!(convert_jex_note_body(NOTE, "atoms.md", 1, body, &BTreeMap::new()).is_err());
    }
    let resources = BTreeMap::from([(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        app_lite_core::JexVerifiedResource {
            destination_id: app_lite_core::ResourceId::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .unwrap(),
            mime: "image/png".into(),
            filename: "图.png".into(),
        },
    )]);
    for (markup, untitled, titled) in [
        (
            1,
            "[![图](:/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)](https://example.test)",
            "[![图](:/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)](https://example.test \"tip\")",
        ),
        (
            2,
            "<p><a href=\"https://example.test\"><img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"图\"></a></p>",
            "<p><a href=\"https://example.test\" title=\"tip\"><img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"图\"></a></p>",
        ),
    ] {
        assert!(convert_jex_note_body(NOTE, "image.md", markup, untitled, &resources).is_ok());
        assert!(
            convert_jex_note_body(NOTE, "image.md", markup, titled, &resources).is_err(),
            "the image model has no title; strict conversion cannot discard it"
        );
    }
}

#[test]
fn repeated_styled_runs_cannot_amplify_retained_link_metadata() {
    let title = "x".repeat(4096);
    let html = format!(
        "<p><a href=\"https://example.test\" title=\"{title}\">{}</a></p>",
        "<strong>a</strong><em>b</em>".repeat(40)
    );
    let canonical = CanonicalDocument::parse_html(&html).unwrap();
    assert_eq!(canonical.search_text().as_str(), "ab".repeat(40));
    assert!(canonical.to_canonical_html().as_str().len() < 80_000);
    for markup in [1, 2] {
        assert!(
            convert_jex_note_body(NOTE, "budget.html", markup, &html, &BTreeMap::new()).is_err(),
            "strict import must report exceeding the retention budget instead of declaring lossless success"
        );
    }
    assert!(convert_enml(&format!("<en-note>{html}</en-note>"), &BTreeMap::new()).is_err());
}

#[test]
fn saved_link_title_survives_reopen_and_full_readable_backup_restore() {
    use app_lite_core::{
        CreateNote, LibraryRepository, SaveNote, export_library_readable, restore_library_readable,
    };
    use std::fs;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let path = source.join("library.sqlite");
    let repo = LibraryRepository::open(&path).unwrap();
    let note = repo
        .create_note(CreateNote {
            title: "测试链接".into(),
            notebook_id: None,
            document: CanonicalDocument::parse_html("<p>初始</p>").unwrap(),
        })
        .unwrap();
    repo.save_note(SaveNote {
        id: note.id.clone(),
        expected_revision: note.revision,
        title: note.title,
        document: CanonicalDocument::parse_html(HTML).unwrap(),
        resource_ids: vec![],
        selected_thumbnail_id: None,
    })
    .unwrap();
    drop(repo);
    let repo = LibraryRepository::open(&path).unwrap();
    assert_eq!(
        repo.load_note(&note.id)
            .unwrap()
            .unwrap()
            .body_html
            .as_str(),
        HTML
    );
    let bundle = root.path().join("backup");
    export_library_readable(&repo, &bundle).unwrap();
    let readable =
        fs::read_to_string(bundle.join(format!("readable/{}.html", note.id.as_str()))).unwrap();
    assert!(
        readable.contains(HTML),
        "readable export must retain the link attribute and escaped title"
    );
    let restored = root.path().join("restored");
    fs::create_dir(&restored).unwrap();
    restore_library_readable(&bundle, &restored).unwrap();
    let restored = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    assert_eq!(
        restored
            .load_note(&note.id)
            .unwrap()
            .unwrap()
            .body_html
            .as_str(),
        HTML
    );
}

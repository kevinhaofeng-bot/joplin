//! Native acceptance 151: a saved canonical table must not make a readable
//! selection or entire-library export fail. Keep the table and resource links,
//! then round-trip the recovery data without flattening cells.

use app_lite_core::{
    CanonicalDocument, CreateNote, LibraryRepository, export_library_readable,
    export_readable_selection, restore_library_readable, restore_readable_export,
};
use std::fs;
use tempfile::tempdir;

fn table_note(repo: &LibraryRepository) -> app_lite_core::Note {
    let image = repo
        .import_resource(
            b"table image original bytes",
            "table.png",
            "image/png",
            "png",
        )
        .unwrap();
    let html = format!(
        "<p>表格前</p><table><tbody><tr><th>标题甲</th><th>标题乙</th></tr><tr><td><strong>格内中文</strong><br><img src=\":/{}\" alt=\"格内图\" data-joplin-lite-display-width=\"145\"></td><td>末格</td></tr></tbody></table><p>表格后</p>",
        image.as_str()
    );
    let document = CanonicalDocument::parse_html(&html).unwrap();
    assert!(document.to_canonical_html().as_str().contains("<table"));
    repo.create_note(CreateNote {
        title: "可读表格验收".into(),
        notebook_id: None,
        document,
    })
    .unwrap()
}

fn assert_browser_table(page: &str) {
    for expected in [
        "<table",
        "<tbody>",
        "<tr>",
        "<th>",
        "<td>",
        "<strong>格内中文</strong>",
        "表格前",
        "表格后",
        "末格",
        "../resources/",
        "table{border-collapse:collapse;",
        "td,th{border:1px solid",
        "vertical-align:top",
        "td img,th img{max-width:100%;height:auto}",
    ] {
        assert!(page.contains(expected), "readable page lost {expected}");
    }
    assert!(
        !page.contains("src=\":/"),
        "browser image needs a real resource path"
    );
    assert!(page.contains("width:145px"), "preserve image display size");
}

#[test]
fn whole_library_readable_table_keeps_cells_images_and_recoverable_structure() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("source")).unwrap();
    let repo = LibraryRepository::open(root.path().join("source/library.sqlite")).unwrap();
    let note = table_note(&repo);
    let canonical = note.body_html.clone();
    let bundle = root.path().join("whole");
    export_library_readable(&repo, &bundle)
        .expect("canonical tables must export, not abort the library");
    let page =
        fs::read_to_string(bundle.join(format!("readable/{}.html", note.id.as_str()))).unwrap();
    assert_browser_table(&page);
    let restored = root.path().join("restored");
    fs::create_dir(&restored).unwrap();
    restore_library_readable(&bundle, &restored).unwrap();
    let restored = LibraryRepository::open(restored.join("library.sqlite")).unwrap();
    assert_eq!(
        restored.load_note(&note.id).unwrap().unwrap().body_html,
        canonical
    );
}

#[test]
fn selected_readable_table_keeps_cells_images_and_recoverable_structure() {
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("source")).unwrap();
    let repo = LibraryRepository::open(root.path().join("source/library.sqlite")).unwrap();
    let note = table_note(&repo);
    let canonical = note.body_html.clone();
    let bundle = root.path().join("selection");
    export_readable_selection(&repo, &[note.id.clone()], &bundle)
        .expect("canonical tables must also export through the selection path");
    let page =
        fs::read_to_string(bundle.join(format!("readable/{}.html", note.id.as_str()))).unwrap();
    assert_browser_table(&page);
    let restored_path = root.path().join("restored");
    fs::create_dir(&restored_path).unwrap();
    restore_readable_export(&bundle, &restored_path).unwrap();
    let restored = LibraryRepository::open(restored_path.join("library.sqlite")).unwrap();
    assert_eq!(
        restored.load_note(&note.id).unwrap().unwrap().body_html,
        canonical
    );
}

#[test]
fn both_readable_export_paths_accept_the_canonical_style_vocabulary() {
    // The serializer already emits these tags. Export must preserve them,
    // not reject a note which the editor and repository can already save.
    let cases = [
        ("h4", "<h4>四级标题</h4>"),
        ("h5", "<h5>五级标题</h5>"),
        ("h6", "<h6>六级标题</h6>"),
        ("sup", "<p>平方<sup>2</sup></p>"),
        ("sub", "<p>水H<sub>2</sub>O</p>"),
        (
            "span",
            "<p><span style=\"color:#e03131\">红色中文</span></p>",
        ),
    ];
    let mut failures = Vec::new();
    for (tag, html) in cases {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("source")).unwrap();
        let repo = LibraryRepository::open(root.path().join("source/library.sqlite")).unwrap();
        let document = CanonicalDocument::parse_html(html).unwrap();
        assert!(
            document
                .to_canonical_html()
                .as_str()
                .contains(&format!("<{tag}"))
        );
        let note = repo
            .create_note(CreateNote {
                title: format!("可读样式 {tag}"),
                notebook_id: None,
                document,
            })
            .unwrap();
        let canonical = note.body_html.clone();
        for whole in [false, true] {
            let destination = root.path().join(if whole { "whole" } else { "selection" });
            let result = if whole {
                export_library_readable(&repo, &destination).map(|_| ())
            } else {
                export_readable_selection(&repo, &[note.id.clone()], &destination).map(|_| ())
            };
            if let Err(error) = result {
                failures.push(format!("{tag}, whole={whole}: {error:?}"));
                continue;
            }
            let page =
                fs::read_to_string(destination.join(format!("readable/{}.html", note.id.as_str())))
                    .unwrap();
            assert!(page.contains(&canonical), "browser output lost {tag}");
            let restored_path = root.path().join(if whole {
                "restored-whole"
            } else {
                "restored-selection"
            });
            fs::create_dir(&restored_path).unwrap();
            if whole {
                restore_library_readable(&destination, &restored_path).unwrap();
            } else {
                restore_readable_export(&destination, &restored_path).unwrap();
            }
            let restored = LibraryRepository::open(restored_path.join("library.sqlite")).unwrap();
            assert_eq!(
                restored.load_note(&note.id).unwrap().unwrap().body_html,
                canonical
            );
        }
    }
    assert!(
        failures.is_empty(),
        "canonical export failures: {failures:#?}"
    );
}

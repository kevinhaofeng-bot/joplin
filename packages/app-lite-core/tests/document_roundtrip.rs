use app_lite_core::document::{Block, BlockStyle, Inline, Marks};
use app_lite_core::{CanonicalDocument, ResourceId};

const FIRST_RESOURCE: &str = "0123456789abcdef0123456789abcdef";
const SECOND_RESOURCE: &str = "fedcba9876543210fedcba9876543210";

#[test]
fn canonical_document_preserves_headings_marks_lists_checks_and_resource_order() {
    // Catches a projection that drops semantic blocks, visible marks, or duplicate image references.
    let input = format!(
        "<h2 data-align=\"center\">Title <strong>bold</strong></h2><ul data-type=\"checklist\"><li data-checked=\"true\">done<img src=\":/{FIRST_RESOURCE}\" alt=\"receipt\"></li><li data-checked=\"false\">todo<ul><li>nested<img src=\":/{SECOND_RESOURCE}\" alt=\"diagram\"></li></ul>again<img src=\":/{FIRST_RESOURCE}\" alt=\"receipt again\"></li></ul>"
    );

    let parsed = CanonicalDocument::parse_html(&input).unwrap();
    let html = parsed.to_canonical_html();
    assert_eq!(
        CanonicalDocument::parse_html(html.as_str()).unwrap(),
        parsed
    );
    assert_eq!(
        parsed.search_text().as_str(),
        "Title bold\ndonereceipt\ntodo\nnesteddiagram\nagainreceipt again"
    );
    assert_eq!(
        parsed.resource_ids(),
        vec![
            ResourceId::new(FIRST_RESOURCE).unwrap(),
            ResourceId::new(SECOND_RESOURCE).unwrap(),
            ResourceId::new(FIRST_RESOURCE).unwrap(),
        ]
    );
}

#[test]
fn canonical_document_discards_invalid_html_nodes_and_unsafe_resource_references() {
    // Catches unsafe markup leaking into canonical HTML or invalid images becoming resources.
    let parsed = CanonicalDocument::parse_html(&format!(
        "<p onclick=\"alert(1)\"><script>secret()</script><a href=\"javascript:alert(1)\">visible</a><img src=\"https://example.com/no.png\" alt=\"remote\"><img src=\":/{FIRST_RESOURCE}\" alt=\"local\"></p>"
    ))
    .unwrap();

    assert_eq!(parsed.search_text().as_str(), "visibleremotelocal");
    assert_eq!(
        parsed.resource_ids(),
        vec![ResourceId::new(FIRST_RESOURCE).unwrap()]
    );
    let html = parsed.to_canonical_html();
    assert!(!html.as_str().contains("script"));
    assert!(!html.as_str().contains("onclick"));
    assert!(!html.as_str().contains("javascript:"));
    assert_eq!(
        CanonicalDocument::parse_html(html.as_str()).unwrap(),
        parsed
    );
}

#[test]
fn canonical_document_projects_one_hundred_thousand_visible_characters() {
    // Catches an extraction byte/DOM cap that silently truncates ordinary long notes.
    let input = format!("<p>{}</p>", "x".repeat(100_000));
    let parsed = CanonicalDocument::parse_html(&input).unwrap();
    assert_eq!(parsed.search_text().as_str().len(), 100_000);
    assert_eq!(
        CanonicalDocument::parse_html(parsed.to_canonical_html().as_str()).unwrap(),
        parsed
    );
}

#[test]
fn public_constructor_keeps_document_canonical_and_rejects_invalid_image_ids() {
    // Catches public construction that can defer normalization until serialization.
    let valid = ResourceId::new(FIRST_RESOURCE).unwrap();
    assert!(ResourceId::new("not-a-resource-id").is_err());
    assert!(ResourceId::new("a".repeat(64)).is_err());

    let document = CanonicalDocument::from_blocks(vec![Block::Paragraph {
        style: BlockStyle {
            indent: 99,
            ..BlockStyle::default()
        },
        inlines: vec![
            Inline::Image {
                resource_id: valid,
                alt: "receipt".into(),
                display_width: None,
                link: None,
            },
            Inline::Text {
                text: " linked".into(),
                marks: Marks {
                    link: Some("javascript:alert(1)".into()),
                    ..Marks::default()
                },
            },
        ],
    }]);

    assert_eq!(document.blocks().len(), 1);
    let html = document.to_canonical_html();
    assert!(html.as_str().contains("data-indent=\"8\""));
    assert!(!html.as_str().contains("href="));
    assert_eq!(
        CanonicalDocument::parse_html(html.as_str()).unwrap(),
        document
    );
}

/// Handoff stage 1: an image inside a heading/quote/list keeps a user width.
/// Evernote stores `width` on the image node itself (resource/image/
/// imagecomponent.tsx:449-452), independent of the parent block.
#[test]
fn inline_image_display_width_round_trips_and_old_html_has_none() {
    use app_lite_core::document::{Block, Inline};
    let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let html = format!(
        "<h2>前<img src=\":/{id}\" alt=\"图\" data-joplin-lite-display-width=\"320\">后</h2>"
    );
    let document = app_lite_core::CanonicalDocument::parse_html(&html).unwrap();
    let width = |document: &app_lite_core::CanonicalDocument| match &document.blocks()[0] {
        Block::Heading { inlines, .. } => inlines.iter().find_map(|inline| match inline {
            Inline::Image { display_width, .. } => Some(*display_width),
            _ => None,
        }),
        other => panic!("{other:?}"),
    };
    assert_eq!(width(&document), Some(Some(320)));
    let serialized = document.to_canonical_html();
    let reparsed = app_lite_core::CanonicalDocument::parse_html(serialized.as_str()).unwrap();
    assert_eq!(reparsed, document);
    assert!(
        serialized
            .as_str()
            .contains("data-joplin-lite-display-width=\"320\"")
    );

    let legacy = app_lite_core::CanonicalDocument::parse_html(&format!(
        "<h2>前<img src=\":/{id}\" alt=\"图\">后</h2>"
    ))
    .unwrap();
    assert_eq!(width(&legacy), Some(None));
    assert!(
        !legacy
            .to_canonical_html()
            .as_str()
            .contains("display-width")
    );
}

/// Handoff stage 1: a file attachment can live inside a heading/quote/list
/// item (Evernote list items hold any content, list/schema.ts:412). Legacy
/// block attachment cards are unchanged.
#[test]
fn inline_attachment_round_trips_inside_list_items_and_is_searchable() {
    use app_lite_core::document::{Block, Inline};
    let pdf = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let html = format!(
        "<ul><li>见<a data-joplin-lite-inline-attachment=\"true\" href=\":/{pdf}\" data-filename=\"合同.pdf\" data-media-type=\"application/pdf\">合同.pdf</a>附件</li></ul>"
    );
    let document = app_lite_core::CanonicalDocument::parse_html(&html).unwrap();
    let Block::List { items, .. } = &document.blocks()[0] else {
        panic!("{:?}", document.blocks());
    };
    assert!(matches!(
        items[0].inlines.as_slice(),
        [
            Inline::Text { .. },
            Inline::Attachment { filename, media_type, .. },
            Inline::Text { .. }
        ] if filename == "合同.pdf" && media_type == "application/pdf"
    ));
    assert_eq!(
        document
            .resource_ids()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        vec![pdf]
    );
    assert!(document.search_text().as_str().contains("合同.pdf"));
    let serialized = document.to_canonical_html();
    assert_eq!(
        app_lite_core::CanonicalDocument::parse_html(serialized.as_str()).unwrap(),
        document
    );

    // A block-level card keeps its block representation.
    let block = app_lite_core::CanonicalDocument::parse_html(&format!(
        "<a data-joplin-lite-block-attachment=\"true\" href=\":/{pdf}\" data-resource-id=\"{pdf}\" data-filename=\"a.pdf\" data-media-type=\"application/pdf\">a.pdf</a>"
    ))
    .unwrap();
    assert!(matches!(block.blocks()[0], Block::Attachment { .. }));
}

/// Linked images (`<a href><img></a>`, Markdown `[![alt](:/img)](url)`)
/// keep their link on the image itself; unlinked legacy HTML parses to None.
#[test]
fn linked_inline_and_block_images_round_trip_their_link() {
    let inline_html = format!(
        "<p>前<a href=\"https://example.com/x?a=1&amp;b=2\"><img src=\":/{FIRST_RESOURCE}\" alt=\"图\"></a>后</p>"
    );
    let document = CanonicalDocument::parse_html(&inline_html).unwrap();
    match &document.blocks()[0] {
        Block::Paragraph { inlines, .. } => assert!(inlines.iter().any(|inline| matches!(
            inline,
            Inline::Image { link: Some(link), .. } if link == "https://example.com/x?a=1&b=2"
        ))),
        other => panic!("{other:?}"),
    }
    assert_eq!(document.to_canonical_html().as_str(), inline_html);

    let block_html = format!(
        "<a href=\"https://example.com/y\"><img data-joplin-lite-block-image=\"true\" src=\":/{SECOND_RESOURCE}\" alt=\"块图\"></a>"
    );
    let document = CanonicalDocument::parse_html(&block_html).unwrap();
    assert!(matches!(
        document.blocks(),
        [Block::Image { link: Some(link), .. }] if link == "https://example.com/y"
    ));
    assert_eq!(document.to_canonical_html().as_str(), block_html);

    let legacy = CanonicalDocument::parse_html(&format!(
        "<p><img src=\":/{FIRST_RESOURCE}\" alt=\"旧\"></p><img data-joplin-lite-block-image=\"true\" src=\":/{SECOND_RESOURCE}\" alt=\"旧块\">"
    ))
    .unwrap();
    assert!(matches!(
        legacy.blocks(),
        [Block::Paragraph { inlines, .. }, Block::Image { link: None, .. }]
            if matches!(inlines.as_slice(), [Inline::Image { link: None, .. }])
    ));
}

#[test]
fn unsafe_image_links_are_dropped_not_serialized() {
    let document = CanonicalDocument::parse_html(&format!(
        "<p><a href=\"javascript:alert(1)\"><img src=\":/{FIRST_RESOURCE}\" alt=\"x\"></a></p>"
    ))
    .unwrap();
    let html = document.to_canonical_html();
    assert!(!html.as_str().contains("javascript"), "{}", html.as_str());
    assert!(!html.as_str().contains("<a "), "{}", html.as_str());

    let constructed = CanonicalDocument::from_blocks(vec![Block::Image {
        resource_id: ResourceId::new(FIRST_RESOURCE).unwrap(),
        alt: "x".into(),
        presentation: Default::default(),
        link: Some("javascript:alert(1)".into()),
    }]);
    assert!(matches!(
        constructed.blocks(),
        [Block::Image { link: None, .. }]
    ));
    assert!(!constructed.to_canonical_html().as_str().contains("<a "));
}

/// Evernote and Markdown notes use h4–h6; they stay headings instead of
/// being flattened into paragraphs.
#[test]
fn h4_to_h6_round_trip_as_headings() {
    use app_lite_core::document::HeadingLevel;
    let html = "<h4>四</h4><h5 data-align=\"center\">五</h5><h6>六</h6>";
    let document = CanonicalDocument::parse_html(html).unwrap();
    let levels: Vec<_> = document
        .blocks()
        .iter()
        .map(|block| match block {
            Block::Heading { level, .. } => *level,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        levels,
        vec![HeadingLevel::Four, HeadingLevel::Five, HeadingLevel::Six]
    );
    assert_eq!(document.to_canonical_html().as_str(), html);
}

/// `<ol start="N">` keeps its first number; start 1 and unordered lists
/// carry none, so existing bodies serialize byte-for-byte as before.
#[test]
fn ordered_list_start_round_trips_and_keeps_adjacent_lists_apart() {
    let html = "<ol start=\"3\"><li>三</li><li>四</li></ol><ol start=\"10\"><li>十</li></ol>";
    let document = CanonicalDocument::parse_html(html).unwrap();
    let starts: Vec<_> = document
        .blocks()
        .iter()
        .map(|block| match block {
            Block::List { start, .. } => *start,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(starts, vec![Some(3), Some(10)]);
    assert_eq!(document.to_canonical_html().as_str(), html);

    for (input, expected) in [
        ("<ol start=\"1\"><li>一</li></ol>", "<ol><li>一</li></ol>"),
        ("<ol start=\"-2\"><li>一</li></ol>", "<ol><li>一</li></ol>"),
        ("<ul start=\"3\"><li>点</li></ul>", "<ul><li>点</li></ul>"),
        (
            "<ol><li>一</li></ol><ol><li>二</li></ol>",
            "<ol><li>一</li><li>二</li></ol>",
        ),
    ] {
        let document = CanonicalDocument::parse_html(input).unwrap();
        assert_eq!(document.to_canonical_html().as_str(), expected, "{input}");
    }
}

/// GFM-style tables: rows of inline-only cells, first row optionally a
/// header. Spans, block content and nested tables stay on the generic path.
#[test]
fn simple_tables_round_trip_with_header_links_images_and_breaks() {
    use app_lite_core::document::TableCell;
    let html = format!(
        "<table data-joplin-lite-table=\"true\"><tbody><tr><th>名称</th><th>说明</th></tr><tr><td><strong>甲</strong>|乙</td><td>一行<br>二行</td></tr><tr><td><a href=\"https://example.com/\">链接</a></td><td><img src=\":/{FIRST_RESOURCE}\" alt=\"图\"></td></tr></tbody></table>"
    );
    let document = CanonicalDocument::parse_html(&html).unwrap();
    let [Block::Table { rows, header: true }] = document.blocks() else {
        panic!("{:?}", document.blocks());
    };
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.cells.len() == 2));
    assert!(matches!(
        rows[2].cells[1],
        TableCell { ref inlines } if matches!(inlines.as_slice(), [Inline::Image { .. }])
    ));
    assert_eq!(document.to_canonical_html().as_str(), html);
    assert_eq!(
        document.search_text().as_str(),
        "名称\t说明\n甲|乙\t一行\n二行\n链接\t图"
    );
    assert_eq!(
        document.resource_ids(),
        vec![ResourceId::new(FIRST_RESOURCE).unwrap()]
    );
}

#[test]
fn plain_html_tables_are_normalized_and_ragged_rows_padded() {
    let document = CanonicalDocument::parse_html(
        "<table><thead><tr><td>a</td><td>b</td></tr></thead><tr><td>c</td></tr></table>",
    )
    .unwrap();
    assert_eq!(
        document.to_canonical_html().as_str(),
        "<table data-joplin-lite-table=\"true\"><tbody><tr><td>a</td><td>b</td></tr><tr><td>c</td><td></td></tr></tbody></table>"
    );
}

#[test]
fn tables_with_spans_or_block_cells_keep_the_generic_path() {
    for html in [
        "<table><tr><td colspan=\"2\">a</td></tr></table>",
        "<table><tr><td><ul><li>a</li></ul></td></tr></table>",
    ] {
        let document = CanonicalDocument::parse_html(html).unwrap();
        assert!(
            !document
                .blocks()
                .iter()
                .any(|block| matches!(block, Block::Table { .. })),
            "{html}"
        );
        assert!(document.search_text().as_str().contains('a'), "{html}");
    }
    // The outer table falls back; the inner simple table keeps its grid.
    let nested = CanonicalDocument::parse_html(
        "<table><tr><td>外<table><tr><td>内</td></tr></table></td></tr></table>",
    )
    .unwrap();
    assert_eq!(nested.search_text().as_str(), "外\n内");
    assert!(matches!(
        nested.blocks(),
        [Block::Paragraph { .. }, Block::Table { rows, .. }] if rows.len() == 1
    ));
}

#[test]
fn superscript_and_subscript_round_trip_and_exclude_each_other() {
    use app_lite_core::Script;
    // Each run carries all of its tags, as every other mark does.
    let html = "<p>H<sub>2</sub>O and <strong>x</strong><strong><sup>2</sup></strong></p>";
    let parsed =
        CanonicalDocument::parse_html("<p>H<sub>2</sub>O and <strong>x<sup>2</sup></strong></p>")
            .unwrap();
    assert_eq!(parsed.to_canonical_html().as_str(), html);
    assert_eq!(
        CanonicalDocument::parse_html(html)
            .unwrap()
            .to_canonical_html()
            .as_str(),
        html
    );
    let Block::Paragraph { inlines, .. } = &parsed.blocks()[0] else {
        panic!("paragraph");
    };
    assert!(inlines.iter().any(|inline| matches!(
        inline,
        Inline::Text { text, marks } if text == "2" && marks.script == Some(Script::Subscript)
    )));
    // Evernote's marks exclude each other: the inner one wins.
    let nested = CanonicalDocument::parse_html("<p><sup>a<sub>b</sub></sup></p>").unwrap();
    assert_eq!(
        nested.to_canonical_html().as_str(),
        "<p><sup>a</sup><sub>b</sub></p>"
    );
    // HTML written before this mark existed reads the same as before.
    let old = "<p>plain <strong>bold</strong></p>";
    assert_eq!(
        CanonicalDocument::parse_html(old)
            .unwrap()
            .to_canonical_html()
            .as_str(),
        old
    );
    let _ = Marks::default();
}

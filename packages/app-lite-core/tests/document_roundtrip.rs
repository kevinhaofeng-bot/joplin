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

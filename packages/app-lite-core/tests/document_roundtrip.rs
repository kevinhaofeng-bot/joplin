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

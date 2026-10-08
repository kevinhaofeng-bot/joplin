//! Independent clipboard fidelity checks. Collapsing background colors to a
//! boolean loses document information before the native editor can render it.

use app_lite_core::CanonicalDocument;
use std::collections::BTreeSet;

#[test]
fn legacy_palette_and_default_marks_remain_readable() {
    for (old, current) in [
        ("#ffef9e", "#fdf3d0"),
        ("#fec1d0", "#ffe2d5"),
        ("#b7f7d1", "#ddf8e1"),
        ("#adecf4", "#e0f7fd"),
        ("#cbcaff", "#edf0ff"),
        ("#ffd1b0", "#feead4"),
        ("#fffaa5", "#fdf3d0"),
        ("#ffcc66", "#fdf3d0"),
        ("#f6ee96", "#fdf3d0"),
    ] {
        let doc = CanonicalDocument::parse_pasted_html(&format!(
            "<p><span style=\"background-color:{old}\">旧</span></p>"
        ))
        .unwrap()
        .document;
        assert_eq!(
            doc.to_canonical_html().as_str(),
            format!("<p><mark style=\"background-color: {current}\">旧</mark></p>")
        );
    }
    let legacy = CanonicalDocument::parse_html("<p><mark>旧默认</mark></p>").unwrap();
    assert_eq!(
        legacy.to_canonical_html().as_str(),
        "<p><mark>旧默认</mark></p>"
    );
}

#[test]
fn background_css_is_validated_and_table_colors_survive_roundtrip() {
    for invalid in ["url(https://example.test/x)", "currentcolor", "not-a-color"] {
        let doc = CanonicalDocument::parse_pasted_html(&format!(
            "<p><span style=\"background-color:{invalid}\">普通</span></p>"
        ))
        .unwrap()
        .document;
        assert_eq!(doc.to_canonical_html().as_str(), "<p>普通</p>");
    }
    let doc = CanonicalDocument::parse_pasted_html(
        "<table><tr><td><span style=\"background-color:rgba(12,34,56,0.5)\">单元格</span></td><td><span style=\"background-color:#ddf8e1\">绿</span></td></tr></table>",
    ).unwrap().document;
    let html = doc.to_canonical_html();
    assert!(html.as_str().contains("background-color:"));
    assert!(html.as_str().contains("#ddf8e1"));
    assert_eq!(CanonicalDocument::parse_html(html.as_str()).unwrap(), doc);
}

#[test]
fn pasted_highlight_is_not_plain_text_after_canonical_roundtrip() {
    let plain = CanonicalDocument::parse_pasted_html("<p>同一段中文</p>")
        .unwrap()
        .document;
    let highlighted = CanonicalDocument::parse_pasted_html(
        "<p><span style=\"background-color:#fdf3d0\">同一段中文</span></p>",
    )
    .unwrap()
    .document;
    assert_eq!(plain.search_text().as_str(), "同一段中文");
    assert_eq!(highlighted.search_text().as_str(), "同一段中文");
    let html = highlighted.to_canonical_html();
    let reopened = CanonicalDocument::parse_html(html.as_str()).unwrap();
    assert_ne!(reopened.to_canonical_html(), plain.to_canonical_html());
}

#[test]
fn adjacent_highlights_keep_colors_and_plain_suffix() {
    let doc = CanonicalDocument::parse_pasted_html(
        "<p><span style=\"background-color:#ffe2d5\">甲</span><span style=\"background-color:#ddf8e1\">乙</span>丙</p>",
    ).unwrap().document;
    let html = doc.to_canonical_html();
    let reopened = CanonicalDocument::parse_html(html.as_str()).unwrap();
    assert_eq!(reopened.search_text().as_str(), "甲乙丙");
    assert_eq!(html, reopened.to_canonical_html());
    assert_eq!(
        reopened.to_canonical_html().as_str(),
        "<p><mark style=\"background-color: #ffe2d5\">甲</mark><mark style=\"background-color: #ddf8e1\">乙</mark>丙</p>"
    );
}

#[test]
fn nested_highlights_override_and_transparent_clears_only_inner_range() {
    let doc = CanonicalDocument::parse_pasted_html(
        "<p><span style=\"background-color:#ffe2d5\">甲<span style=\"background-color:#e0f7fd\">乙</span><span style=\"background-color:transparent\">丙</span>丁</span>戊</p>",
    ).unwrap().document;
    let reopened = CanonicalDocument::parse_html(doc.to_canonical_html().as_str()).unwrap();
    assert_eq!(reopened.search_text().as_str(), "甲乙丙丁戊");
    assert_eq!(
        reopened.to_canonical_html().as_str(),
        "<p><mark style=\"background-color: #ffe2d5\">甲</mark><mark style=\"background-color: #e0f7fd\">乙</mark>丙<mark style=\"background-color: #ffe2d5\">丁</mark>戊</p>"
    );
}

#[test]
fn six_pasted_highlight_colors_remain_distinct_after_canonical_roundtrip() {
    // Same text, six different highlight colors. No particular HTML syntax is
    // required, but persistence must distinguish these six visible documents.
    // Values were checked against the original Evernote 11.32.5 source map.
    let colors = [
        "#fdf3d0", "#ffe2d5", "#ddf8e1", "#e0f7fd", "#edf0ff", "#feead4",
    ];
    let mut persisted = BTreeSet::new();
    for color in colors {
        let source = format!("<p><span style=\"background-color:{color}\">同一段中文</span></p>");
        let pasted = CanonicalDocument::parse_pasted_html(&source)
            .unwrap()
            .document;
        assert_eq!(pasted.search_text().as_str(), "同一段中文");
        let html = pasted.to_canonical_html();
        let reopened = CanonicalDocument::parse_html(html.as_str()).unwrap();
        assert_eq!(reopened.search_text().as_str(), "同一段中文");
        persisted.insert(reopened.to_canonical_html().as_str().to_owned());
    }
    assert_eq!(
        persisted.len(),
        6,
        "six visible highlight colors must not collapse to the same saved document: {persisted:?}"
    );
}

use std::collections::BTreeMap;

use app_lite_core::{
    CanonicalDocument, JexBodyBlockerKind, JexVerifiedResource, ResourceId, convert_jex_note_body,
};

const NOTE: &str = "11111111111111111111111111111111";
const IMAGE: &str = "222222222222222222222222222222aa";
const PDF: &str = "333333333333333333333333333333bb";
const TARGET_IMAGE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET_PDF: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn resources() -> BTreeMap<String, JexVerifiedResource> {
    BTreeMap::from([
        (
            IMAGE.to_ascii_uppercase(),
            JexVerifiedResource {
                destination_id: ResourceId::new(TARGET_IMAGE).unwrap(),
                mime: "image/png".into(),
                filename: "元数据图.png".into(),
            },
        ),
        (
            PDF.into(),
            JexVerifiedResource {
                destination_id: ResourceId::new(TARGET_PDF).unwrap(),
                mime: "application/pdf".into(),
                filename: "附件.pdf".into(),
            },
        ),
    ])
}

#[test]
fn converts_conservative_html_with_exact_text_structure_and_occurrences() {
    // Mutation caught: a permissive HTML projection dropping nodes, marks, or
    // text while still round-tripping its own already-lossy output.
    let source = format!(
        "<html><body><h2>中文标题</h2><p>前<strong>粗</strong><em>斜</em><u>下</u><s>删</s><mark>亮</mark><code>码</code><br/>后 <a href=\"https://example.com/?a=1&amp;b=2\">外链</a></p><div>图前<img src=\":/{}\" alt=\"图\"/>中<img src=\":/{IMAGE}\" alt=\"图\"/>图后</div><ul><li>一</li><li>二</li></ul><ul data-type=\"checklist\"><li data-checked=\"true\">已办</li><li data-checked=\"false\">未办</li></ul><p><a href=\":/{PDF}\">附件.pdf</a></p></body></html>",
        IMAGE.to_ascii_uppercase()
    );
    let converted =
        convert_jex_note_body(NOTE, "html-note.html", 2, &source, &resources()).unwrap();
    assert_eq!(
        converted.canonical_html,
        format!(
            "<h2>中文标题</h2><p>前<strong>粗</strong><em>斜</em><u>下</u><s>删</s><mark>亮</mark><code>码</code><br>后 <a href=\"https://example.com/?a=1&amp;b=2\">外链</a></p><p>图前<img src=\":/{TARGET_IMAGE}\" alt=\"图\">中<img src=\":/{TARGET_IMAGE}\" alt=\"图\">图后</p><ul><li>一</li><li>二</li></ul><ul data-type=\"checklist\"><li data-checked=\"true\">已办</li><li data-checked=\"false\">未办</li></ul><a data-joplin-lite-block-attachment=\"true\" href=\":/{TARGET_PDF}\" data-resource-id=\"{TARGET_PDF}\" data-filename=\"附件.pdf\" data-media-type=\"application/pdf\">附件.pdf</a>"
        )
    );
    assert_eq!(
        converted.search_text,
        "中文标题\n前粗斜下删亮码\n后 外链\n图前图中图图后\n一\n二\n已办\n未办\n附件.pdf"
    );
    assert_eq!(
        converted.ordered_resource_occurrences,
        vec![
            ResourceId::new(TARGET_IMAGE).unwrap(),
            ResourceId::new(TARGET_IMAGE).unwrap(),
            ResourceId::new(TARGET_PDF).unwrap()
        ]
    );
    assert_eq!(
        CanonicalDocument::parse_html(&converted.canonical_html).unwrap(),
        converted.document
    );
}

#[test]
fn rejects_unsupported_html_before_a_lossy_projection() {
    let cases: Vec<(String, JexBodyBlockerKind)> = vec![
        (
            "<table><tr><td>证据</td></tr></table>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<p style=\"color:red\">红色</p>".into(),
            JexBodyBlockerKind::UnsupportedAttribute,
        ),
        (
            "<p class=\"important\">正文</p>".into(),
            JexBodyBlockerKind::UnsupportedAttribute,
        ),
        (
            "<font color=\"red\">红色</font>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<script>alert(1)</script>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<style>p{color:red}</style>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<mystery>会被普通投影丢掉</mystery>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<input type=\"checkbox\"/>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<h4>四级</h4>".into(),
            JexBodyBlockerKind::UnsupportedHeading,
        ),
        (
            "<html><head><title>丢失</title></head><body><p>正文</p></body></html>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<p><a href=\"https://example.com\">外<a href=\"https://example.org\">内</a></a></p>"
                .into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            format!(
                "<p><a href=\"https://example.com\"><img src=\":/{IMAGE}\" alt=\"图\"/></a></p>"
            ),
            JexBodyBlockerKind::LinkedImage,
        ),
        (
            format!("<p><img src=\":/{NOTE}\" alt=\"图\"/></p>"),
            JexBodyBlockerKind::UnverifiedResource,
        ),
        (
            "<p><img src=\"https://example.com/a.png\" alt=\"图\"/></p>".into(),
            JexBodyBlockerKind::UnsupportedImageSource,
        ),
        (
            "<p><img src=\"data:image/png;base64,AAAA\" alt=\"图\"/></p>".into(),
            JexBodyBlockerKind::UnsupportedImageSource,
        ),
        (
            format!("<p><a href=\":/{NOTE}\">笔记</a></p>"),
            JexBodyBlockerKind::InternalNoteLink,
        ),
        (
            format!("<p>前<a href=\":/{PDF}\">附件.pdf</a>后</p>"),
            JexBodyBlockerKind::AmbiguousAttachment,
        ),
        (
            "<p><a href=\"javascript:alert(1)\">执行</a></p>".into(),
            JexBodyBlockerKind::UnsafeLink,
        ),
        (
            "<p><a href=\"../other\">相对</a></p>".into(),
            JexBodyBlockerKind::UnsafeLink,
        ),
        (
            "<ul><li>上<ul><li>下</li></ul></li></ul>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<p>&unknown;</p>".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        ("<p>未闭合".into(), JexBodyBlockerKind::UnsupportedStructure),
    ];
    for (index, (body, kind)) in cases.into_iter().enumerate() {
        let path = format!("folder/{index}/{NOTE}.html");
        let error = convert_jex_note_body(NOTE, &path, 2, &body, &resources()).unwrap_err();
        assert_eq!(error.kind, kind, "body={body}");
        assert_eq!(error.source_note_id, NOTE);
        assert_eq!(error.source_path, path);
        assert!(!error.reason.is_empty());
        assert!(error.reason.len() <= 256);
    }
}

#[test]
fn preserves_escaped_text_and_unicode_whitespace() {
    let body = "<p>甲\u{00a0}乙 &amp; &lt; &quot; 😀 <strong>中\u{2003}文</strong></p>";
    let converted = convert_jex_note_body(NOTE, "unicode.html", 2, body, &resources()).unwrap();
    assert_eq!(converted.search_text, "甲\u{00a0}乙 & < \" 😀 中\u{2003}文");
    assert_eq!(
        converted.canonical_html,
        "<p>甲&nbsp;乙 &amp; &lt; &quot; 😀 <strong>中\u{2003}文</strong></p>"
    );
}

#[test]
fn enforces_html_body_url_and_depth_budgets() {
    let map = resources();
    let oversized = format!("<p>{}</p>", "中".repeat(1_400_000));
    assert_eq!(
        convert_jex_note_body(NOTE, "big.html", 2, &oversized, &map)
            .unwrap_err()
            .kind,
        JexBodyBlockerKind::BodyTooLarge
    );
    let url = format!("https://example.com/{}", "x".repeat(7_800));
    let links = format!("<p>{}</p>", format!("<a href=\"{url}\">链</a>").repeat(9));
    assert_eq!(
        convert_jex_note_body(NOTE, "urls.html", 2, &links, &map)
            .unwrap_err()
            .kind,
        JexBodyBlockerKind::UrlBudget
    );
    let nested = format!("{}深{}", "<b>".repeat(80), "</b>".repeat(80));
    assert_eq!(
        convert_jex_note_body(NOTE, "deep.html", 2, &format!("<p>{nested}</p>"), &map)
            .unwrap_err()
            .kind,
        JexBodyBlockerKind::ParserBudget
    );
}

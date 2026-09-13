use std::collections::BTreeMap;

use app_lite_core::{
    CanonicalDocument, JexBodyBlockerKind, JexVerifiedResource, ResourceId, convert_jex_note_body,
};

const NOTE: &str = "11111111111111111111111111111111";
const IMAGE: &str = "22222222222222222222222222222222";
const PDF: &str = "33333333333333333333333333333333";
const TARGET_IMAGE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET_PDF: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TEXT_FILE: &str = "44444444444444444444444444444444";
const TARGET_TEXT_FILE: &str = "cccccccccccccccccccccccccccccccc";

fn resources() -> BTreeMap<String, JexVerifiedResource> {
    BTreeMap::from([
        (
            IMAGE.into(),
            JexVerifiedResource {
                destination_id: ResourceId::new(TARGET_IMAGE).unwrap(),
                mime: "image/png".into(),
                filename: "元数据文件名.png".into(),
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
        (
            TEXT_FILE.into(),
            JexVerifiedResource {
                destination_id: ResourceId::new(TARGET_TEXT_FILE).unwrap(),
                mime: "text/plain".into(),
                filename: "证据.txt".into(),
            },
        ),
    ])
}

#[test]
fn converts_chinese_crlf_markdown_with_exact_html_search_and_resource_order() {
    // Mutation caught: a lossy HTML projection, Markdown literal fallback,
    // image/text reordering, or metadata filename replacing the body alt.
    let source = format!(
        "# 中文标题\r\n\r\n前**粗** *斜* ~~删~~ `码` [外链](https://example.com/x) 后\r\n换行\r\n\r\n图前![图](:/{IMAGE})图后\r\n\r\n- 清单一\r\n- 清单二\r\n\r\n- [x] 已办\r\n- [ ] 待办\r\n\r\n1. 第一\r\n2. 第二\r\n"
    );
    let converted =
        convert_jex_note_body(NOTE, &format!("{NOTE}.md"), 1, &source, &resources()).unwrap();
    assert_eq!(
        converted.canonical_html,
        format!(
            "<h1>中文标题</h1><p>前<strong>粗</strong> <em>斜</em> <s>删</s> <code>码</code> <a href=\"https://example.com/x\">外链</a> 后<br>换行</p><p>图前<img src=\":/{TARGET_IMAGE}\" alt=\"图\">图后</p><ul><li>清单一</li><li>清单二</li></ul><ul data-type=\"checklist\"><li data-checked=\"true\">已办</li><li data-checked=\"false\">待办</li></ul><ol><li>第一</li><li>第二</li></ol>"
        )
    );
    assert_eq!(
        converted.search_text,
        "中文标题\n前粗 斜 删 码 外链 后\n换行\n图前图图后\n清单一\n清单二\n已办\n待办\n第一\n第二"
    );
    assert_eq!(
        converted.ordered_resource_occurrences,
        vec![ResourceId::new(TARGET_IMAGE).unwrap()]
    );
    assert_eq!(
        CanonicalDocument::parse_html(&converted.canonical_html).unwrap(),
        converted.document
    );
}

#[test]
fn converts_standalone_pdf_card_without_confusing_label_and_metadata() {
    // Mutation caught: treating any internal link as text, or creating a card
    // with a body label that differs from the verified resource filename.
    let source = format!("[附件.pdf](:/{PDF})");
    let converted = convert_jex_note_body(NOTE, "pdf.md", 1, &source, &resources()).unwrap();
    assert_eq!(
        converted.canonical_html,
        format!(
            "<a data-joplin-lite-block-attachment=\"true\" href=\":/{TARGET_PDF}\" data-resource-id=\"{TARGET_PDF}\" data-filename=\"附件.pdf\" data-media-type=\"application/pdf\">附件.pdf</a>"
        )
    );
    assert_eq!(converted.search_text, "附件.pdf");
    assert_eq!(
        converted.ordered_resource_occurrences,
        vec![ResourceId::new(TARGET_PDF).unwrap()]
    );
    assert_eq!(
        CanonicalDocument::parse_html(&converted.canonical_html).unwrap(),
        converted.document
    );
}

#[test]
fn converts_other_verified_non_image_card_and_case_insensitive_source_id() {
    let source = format!("[证据.txt](:/{TEXT_FILE})");
    let converted = convert_jex_note_body(NOTE, "text.md", 1, &source, &resources()).unwrap();
    assert_eq!(converted.search_text, "证据.txt");
    assert_eq!(
        converted.ordered_resource_occurrences,
        vec![ResourceId::new(TARGET_TEXT_FILE).unwrap()]
    );
    assert!(
        converted
            .canonical_html
            .contains("data-media-type=\"text/plain\"")
    );

    let source = format!("![图](:/{})", IMAGE.to_ascii_uppercase());
    let converted = convert_jex_note_body(NOTE, "upper.md", 1, &source, &resources()).unwrap();
    assert_eq!(
        converted.ordered_resource_occurrences,
        vec![ResourceId::new(TARGET_IMAGE).unwrap()]
    );
}

#[test]
fn retains_repeated_image_occurrences_and_chinese_whitespace() {
    // Mutation caught: deduplicating body occurrences or swapping metadata
    // filename for user-authored alt text.
    let source = format!("甲 ![图](:/{IMAGE}) 中 ![图](:/{IMAGE}) 乙");
    let converted = convert_jex_note_body(NOTE, "repeat.md", 1, &source, &resources()).unwrap();
    assert_eq!(
        converted.ordered_resource_occurrences,
        vec![ResourceId::new(TARGET_IMAGE).unwrap(); 2]
    );
    assert_eq!(converted.search_text, "甲 图 中 图 乙");
    assert_eq!(
        converted.canonical_html,
        format!(
            "<p>甲 <img src=\":/{TARGET_IMAGE}\" alt=\"图\"> 中 <img src=\":/{TARGET_IMAGE}\" alt=\"图\"> 乙</p>"
        )
    );
}

#[test]
fn blocks_unsupported_or_lossy_markdown_with_source_location() {
    // Mutation caught: treating a recognized unsupported construct as plain
    // text, or accepting a syntactically plausible but unverified :/ID.
    let cases: Vec<(String, JexBodyBlockerKind)> = vec![
        (
            "| A | B |\n|---|---|\n| 甲 | 乙 |".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "<script>alert(1)</script>".into(),
            JexBodyBlockerKind::RawHtml,
        ),
        (
            "# 好\n\n四级\n----\n\n#### 不能".into(),
            JexBodyBlockerKind::UnsupportedHeading,
        ),
        (
            "- 一级\n  - 二级".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "- 普通\n- [x] 混排".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            "脚注[^a]\n\n[^a]: 正文".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
        (
            ":::note\n插件\n:::".into(),
            JexBodyBlockerKind::PluginDirective,
        ),
        (
            format!("![图](:/{NOTE})"),
            JexBodyBlockerKind::UnverifiedResource,
        ),
        (
            "![图](:/bad-id)".into(),
            JexBodyBlockerKind::UnverifiedResource,
        ),
        (
            format!("[笔记](:/{NOTE})"),
            JexBodyBlockerKind::InternalNoteLink,
        ),
        (
            format!("[![图](:/{IMAGE})](https://example.com)"),
            JexBodyBlockerKind::LinkedImage,
        ),
        (
            "![图](https://example.com/img.png)".into(),
            JexBodyBlockerKind::UnsupportedImageSource,
        ),
        (
            "![图](data:image/png;base64,AAAA)".into(),
            JexBodyBlockerKind::UnsupportedImageSource,
        ),
        (
            "[坏](javascript:alert(1))".into(),
            JexBodyBlockerKind::UnsafeLink,
        ),
        ("[相对](../other.md)".into(), JexBodyBlockerKind::UnsafeLink),
        (
            format!("前[附件.pdf](:/{PDF})后"),
            JexBodyBlockerKind::AmbiguousAttachment,
        ),
        (
            format!("![附件.pdf](:/{PDF})"),
            JexBodyBlockerKind::AmbiguousAttachment,
        ),
        (
            "[外链](https://example.com \"提示\")".into(),
            JexBodyBlockerKind::UnsupportedAttribute,
        ),
        (
            "```mermaid\na-->b\n```".into(),
            JexBodyBlockerKind::UnsupportedStructure,
        ),
    ];
    for (index, (body, kind)) in cases.into_iter().enumerate() {
        let path = format!("folder/{index}/{NOTE}.md");
        let error = convert_jex_note_body(NOTE, &path, 1, &body, &resources()).unwrap_err();
        assert_eq!(error.kind, kind, "body={body}");
        assert_eq!(error.source_note_id, NOTE);
        assert_eq!(error.source_path, path);
        assert!(!error.reason.is_empty());
        assert!(error.reason.len() <= 256);
    }
}

#[test]
fn blocks_html_unknown_markup_and_body_or_url_budgets() {
    let map = resources();
    for (markup, kind) in [
        (2, JexBodyBlockerKind::HtmlNotImplemented),
        (3, JexBodyBlockerKind::UnsupportedMarkupLanguage),
    ] {
        let error =
            convert_jex_note_body(NOTE, "source.md", markup, "<p>正文</p>", &map).unwrap_err();
        assert_eq!(error.kind, kind);
    }
    let oversized = "中".repeat(1_400_000);
    assert_eq!(
        convert_jex_note_body(NOTE, "source.md", 1, &oversized, &map)
            .unwrap_err()
            .kind,
        JexBodyBlockerKind::BodyTooLarge
    );
    let long_url = format!("[链接](https://example.com/{})", "x".repeat(8_200));
    assert_eq!(
        convert_jex_note_body(NOTE, "source.md", 1, &long_url, &map)
            .unwrap_err()
            .kind,
        JexBodyBlockerKind::UrlBudget
    );
}

#[test]
fn blocks_excess_parser_events_before_document_projection() {
    // Mutation caught: limiting only the output HTML after parsing an
    // unbounded event stream.
    let many_items = "- 项\n".repeat(12_000);
    assert_eq!(
        convert_jex_note_body(NOTE, "many.md", 1, &many_items, &resources())
            .unwrap_err()
            .kind,
        JexBodyBlockerKind::ParserBudget
    );
}

#[test]
fn blocks_excess_parser_nesting_before_block_mapping() {
    let nested = format!("{}深", "> ".repeat(80));
    assert_eq!(
        convert_jex_note_body(NOTE, "deep.md", 1, &nested, &resources())
            .unwrap_err()
            .kind,
        JexBodyBlockerKind::ParserBudget
    );
}

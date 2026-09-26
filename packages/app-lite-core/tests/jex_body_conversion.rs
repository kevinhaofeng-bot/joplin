use std::collections::BTreeMap;

use app_lite_core::{
    CanonicalDocument, JexBodyBlockerKind, JexVerifiedResource, ResourceId, convert_jex_note_body,
};

const NOTE: &str = "11111111111111111111111111111111";
const IMAGE: &str = "222222222222222222222222222222aa";
const PDF: &str = "33333333333333333333333333333333";
const TARGET_IMAGE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET_PDF: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TEXT_FILE: &str = "44444444444444444444444444444444";
const TARGET_TEXT_FILE: &str = "cccccccccccccccccccccccccccccccc";

#[test]
fn imports_divider_quote_and_untyped_code_as_native_semantic_blocks() {
    use app_lite_core::document::Block;
    let body = "前文\n\n---\n\n> **引用**\n\n```\nlet x = 1;\n第二行\n```\n\n后文";
    let converted = convert_jex_note_body(NOTE, "blocks.md", 1, body, &resources()).unwrap();
    assert!(matches!(converted.document.blocks()[1], Block::Divider));
    assert!(matches!(
        converted.document.blocks()[2],
        Block::Quote { .. }
    ));
    assert!(matches!(converted.document.blocks()[3], Block::Code { .. }));
    assert!(converted.canonical_html.contains("<strong>引用</strong>"));
    assert!(converted.search_text.contains("let x = 1;\n第二行"));
    assert_eq!(
        CanonicalDocument::parse_html(&converted.canonical_html).unwrap(),
        converted.document
    );
}

#[test]
fn markdown_html_block_preserves_rich_text_instead_of_showing_tags() {
    let body = format!(
        "<div><p><strong>前文</strong></p><p><img src=\":/{IMAGE}\" alt=\"图\"></p></div>\n\n**后文**"
    );
    let (result, _) = app_lite_core::convert_jex_note_body_or_degrade(
        NOTE,
        "html-in-markdown.md",
        1,
        &body,
        &resources(),
    )
    .unwrap();
    assert!(
        result.canonical_html.contains("<strong>前文</strong>"),
        "{}",
        result.canonical_html
    );
    assert!(result.canonical_html.contains("<img "));
    assert!(!result.search_text.contains("<div>"));
}

#[test]
fn unsupported_block_does_not_flatten_surrounding_rich_text_and_images() {
    let body =
        format!("**前文**\n\n![图](:/{IMAGE})\n\n| A | B |\n|---|---|\n| 甲 | 乙 |\n\n**后文**");
    let (result, warning) =
        app_lite_core::convert_jex_note_body_or_degrade(NOTE, "mixed.md", 1, &body, &resources())
            .unwrap();
    assert!(warning.is_some());
    assert!(
        result
            .canonical_html
            .starts_with("<p><strong>前文</strong></p>"),
        "{}",
        result.canonical_html
    );
    assert!(
        result
            .canonical_html
            .ends_with("<p><strong>后文</strong></p>")
    );
    let image = result.canonical_html.find("<img ").unwrap();
    assert!(image < result.canonical_html.find("---").unwrap());
    assert_eq!(
        result.ordered_resource_occurrences,
        vec![ResourceId::new(TARGET_IMAGE).unwrap()]
    );
}

#[test]
fn html_unsupported_container_keeps_neighbor_marks_and_image_position() {
    let body = format!(
        "<div><p><strong>前文</strong></p><blockquote><p>引用</p></blockquote><p><img src=\":/{IMAGE}\" alt=\"图\"></p><p><em>后文</em></p></div>"
    );
    let (result, warning) = app_lite_core::convert_jex_note_body_or_degrade(
        NOTE,
        "mixed-html.md",
        2,
        &body,
        &resources(),
    )
    .unwrap();
    assert!(warning.is_some());
    assert!(result.canonical_html.contains("<strong>前文</strong>"));
    assert!(result.canonical_html.contains("<em>后文</em>"));
    assert!(
        result.canonical_html.find("<img ").unwrap() < result.canonical_html.find("后文").unwrap()
    );
    assert!(!result.search_text.contains("<p>"));
}

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
        "# 中文标题\r\n\r\n前**粗** *斜* ~~删~~ `码` [外链](https://example.com/x) 后\r\n换行\r\n\r\n图前![图](:/{IMAGE})图后\r\n\r\n- 清单一\r\n- 清单二\r\n\r\n列表分界\r\n\r\n- [x] 已办\r\n- [ ] 待办\r\n\r\n1. 第一\r\n2. 第二\r\n"
    );
    let converted =
        convert_jex_note_body(NOTE, &format!("{NOTE}.md"), 1, &source, &resources()).unwrap();
    assert_eq!(
        converted.canonical_html,
        format!(
            "<h1>中文标题</h1><p>前<strong>粗</strong> <em>斜</em> <s>删</s> <code>码</code> <a href=\"https://example.com/x\">外链</a> 后<br>换行</p><p>图前<img src=\":/{TARGET_IMAGE}\" alt=\"图\">图后</p><ul><li>清单一</li><li>清单二</li></ul><p>列表分界</p><ul data-type=\"checklist\"><li data-checked=\"true\">已办</li><li data-checked=\"false\">待办</li></ul><ol><li>第一</li><li>第二</li></ol>"
        )
    );
    assert_eq!(
        converted.search_text,
        "中文标题\n前粗 斜 删 码 外链 后\n换行\n图前图图后\n清单一\n清单二\n列表分界\n已办\n待办\n第一\n第二"
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
fn accepts_uppercase_source_resource_map_key_and_uri_without_losing_occurrence() {
    // Mutation caught: normalizing only the URI, not C2c-1's original map key.
    let mut map = resources();
    let resource = map.remove(IMAGE).unwrap();
    map.insert(IMAGE.to_ascii_uppercase(), resource);
    let source = format!("前![图](:/{})后", IMAGE.to_ascii_uppercase());
    let converted = convert_jex_note_body(NOTE, "upper-map.md", 1, &source, &map).unwrap();
    assert_eq!(converted.search_text, "前图后");
    assert_eq!(
        converted.ordered_resource_occurrences,
        vec![ResourceId::new(TARGET_IMAGE).unwrap()]
    );
}

#[test]
fn rejects_case_fold_collision_in_verified_source_resource_map() {
    // Mutation caught: silently selecting one of two conflicting source keys.
    let mut map = resources();
    map.insert(
        IMAGE.to_ascii_uppercase(),
        JexVerifiedResource {
            destination_id: ResourceId::new(TARGET_PDF).unwrap(),
            mime: "image/png".into(),
            filename: "冲突.png".into(),
        },
    );
    let source = format!("![图](:/{IMAGE})");
    let error = convert_jex_note_body(NOTE, "collision.md", 1, &source, &map).unwrap_err();
    assert_eq!(error.kind, JexBodyBlockerKind::ResourceIdCollision);
    assert_eq!(error.source_note_id, NOTE);
    assert_eq!(error.source_path, "collision.md");
    assert!(error.reason.contains("conflict"));
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
            "- 普通\n\n- [x] 混排".into(),
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
fn blocks_unknown_markup_and_markdown_body_or_url_budgets() {
    let map = resources();
    let error = convert_jex_note_body(NOTE, "source.md", 3, "<p>正文</p>", &map).unwrap_err();
    assert_eq!(error.kind, JexBodyBlockerKind::UnsupportedMarkupLanguage);
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

#[test]
fn empty_and_fragment_links_keep_text_and_tel_links_convert() {
    // 1425 `[t](#…)`/`[t]()` and 410 `tel:` links in the user's export.
    let source = "[空链接]() 与 [锚点](#section) 与 [电话](tel:010-12345678)";
    let conversion = convert_jex_note_body(NOTE, "links.md", 1, source, &resources()).unwrap();
    assert_eq!(
        conversion.canonical_html,
        "<p>空链接 与 锚点 与 <a href=\"tel:010-12345678\">电话</a></p>"
    );
}
#[test]
fn html_semantic_blocks_keep_quote_code_and_divider() {
    use app_lite_core::document::Block;
    let result = convert_jex_note_body(
        "semantic", "semantic.md", 2,
        "<p>before</p><hr><blockquote><p><strong>quote</strong></p></blockquote><pre><code>  first\nsecond\n</code></pre><p>after</p>",
        &BTreeMap::new(),
    ).unwrap();
    assert!(matches!(&result.document.blocks()[1], Block::Divider));
    assert!(matches!(&result.document.blocks()[2], Block::Quote { .. }));
    assert!(matches!(&result.document.blocks()[3], Block::Code { .. }));
    assert!(
        result
            .search_text
            .replace('\u{00a0}', " ")
            .contains("  first\nsecond\n")
    );
    assert_eq!(
        CanonicalDocument::parse_html(&result.canonical_html).unwrap(),
        result.document
    );
}

/// Joplin writes resource links anywhere in text (`[label](:/id)`). They keep
/// their position as inline cards/images; a label that differs from the
/// verified filename stays visible as text before the resource.
#[test]
fn inline_resource_links_keep_position_label_and_resource() {
    let card = |id: &str, name: &str, mime: &str| {
        format!(
            "<a data-joplin-lite-inline-attachment=\"true\" href=\":/{id}\" data-filename=\"{name}\" data-media-type=\"{mime}\">{name}</a>"
        )
    };
    let pdf = card(TARGET_PDF, "附件.pdf", "application/pdf");
    let cases: Vec<(String, String, Vec<&str>)> = vec![
        (
            format!("前[附件.pdf](:/{PDF})后"),
            format!("<p>前{pdf}后</p>"),
            vec![TARGET_PDF],
        ),
        (
            format!("见[合同](:/{PDF})。"),
            format!("<p>见合同{pdf}。</p>"),
            vec![TARGET_PDF],
        ),
        (
            format!("[**合同**](:/{PDF})"),
            format!("<p><strong>合同</strong>{pdf}</p>"),
            vec![TARGET_PDF],
        ),
        (
            format!("**[附件.pdf](:/{PDF})**"),
            format!("<p>{pdf}</p>"),
            vec![TARGET_PDF],
        ),
        (
            format!("[附件.pdf](:/{PDF}) 说明"),
            format!("<p>{pdf} 说明</p>"),
            vec![TARGET_PDF],
        ),
        (
            format!("- 项[附件.pdf](:/{PDF})\n- 二"),
            format!("<ul><li>项{pdf}</li><li>二</li></ul>"),
            vec![TARGET_PDF],
        ),
        (
            format!("## 标题[附件.pdf](:/{PDF})"),
            format!("<h2>标题{pdf}</h2>"),
            vec![TARGET_PDF],
        ),
        (
            format!("看[元数据文件名.png](:/{IMAGE})和[证据.txt](:/{TEXT_FILE})"),
            format!(
                "<p>看<img src=\":/{TARGET_IMAGE}\" alt=\"元数据文件名.png\">和{}</p>",
                card(TARGET_TEXT_FILE, "证据.txt", "text/plain")
            ),
            vec![TARGET_IMAGE, TARGET_TEXT_FILE],
        ),
    ];
    for (body, html, occurrences) in cases {
        let converted = convert_jex_note_body(NOTE, "inline.md", 1, &body, &resources())
            .unwrap_or_else(|error| panic!("body={body}: {error:?}"));
        assert_eq!(converted.canonical_html, html, "body={body}");
        assert_eq!(
            converted.ordered_resource_occurrences,
            occurrences
                .into_iter()
                .map(|id| ResourceId::new(id).unwrap())
                .collect::<Vec<_>>(),
            "body={body}"
        );
        assert_eq!(
            CanonicalDocument::parse_html(&converted.canonical_html).unwrap(),
            converted.document,
            "body={body}"
        );
    }
}

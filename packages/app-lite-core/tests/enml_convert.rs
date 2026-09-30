use std::collections::BTreeMap;

use app_lite_core::document::Block;
use app_lite_core::import_export::{EnmlFidelityBlocker, VerifiedEnmlResource, convert_enml};
use app_lite_core::resource::ResourceId;

const IMAGE_ID: &str = "11111111111111111111111111111111";
const PDF_ID: &str = "22222222222222222222222222222222";
const IMAGE_MD5: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PDF_MD5: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn resources() -> BTreeMap<String, Vec<VerifiedEnmlResource>> {
    BTreeMap::from([
        (
            IMAGE_MD5.into(),
            vec![VerifiedEnmlResource {
                resource_id: ResourceId::new(IMAGE_ID).unwrap(),
                mime: "image/png".into(),
                filename: "图.png".into(),
            }],
        ),
        (
            PDF_MD5.into(),
            vec![VerifiedEnmlResource {
                resource_id: ResourceId::new(PDF_ID).unwrap(),
                mime: "application/pdf".into(),
                filename: "附件.pdf".into(),
            }],
        ),
    ])
}

#[test]
fn converts_ordered_chinese_rich_text_checklist_and_media() {
    // Mutation caught: dropping text around media, marks, checklist states,
    // attachment order, or a verified resource identity changes the result.
    let enml = format!(
        r#"<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd"><en-note><div>前 <b>粗</b> <a href="https://example.com/x">链</a> <en-media hash="{IMAGE_MD5}" type="image/png"/> 后</div><ul><li><en-todo checked="true"/>已办</li><li><en-todo checked="false"/>待办</li></ul><div><en-media hash="{PDF_MD5}" type="application/pdf"/></div></en-note>"#
    );
    let result = convert_enml(&enml, &resources()).unwrap();
    assert_eq!(
        result.html.as_str(),
        format!(
            "<p>前 <strong>粗</strong> <a href=\"https://example.com/x\">链</a> <img src=\":/{IMAGE_ID}\" alt=\"图.png\"> 后</p><ul data-type=\"checklist\"><li data-checked=\"true\">已办</li><li data-checked=\"false\">待办</li></ul><a data-joplin-lite-block-attachment=\"true\" href=\":/{PDF_ID}\" data-resource-id=\"{PDF_ID}\" data-filename=\"附件.pdf\" data-media-type=\"application/pdf\">附件.pdf</a>"
        )
    );
    assert_eq!(
        result.search_text.as_str(),
        "前 粗 链 图.png 后\n已办\n待办\n附件.pdf"
    );
    assert_eq!(
        result
            .resource_ids
            .iter()
            .map(ResourceId::as_str)
            .collect::<Vec<_>>(),
        [IMAGE_ID, PDF_ID]
    );
}

#[test]
fn blocks_unsupported_semantics_and_unverified_media() {
    // Mutation caught: HTML recovery silently flattening a table, or
    // resolving media from outside the explicit same-note verified map.
    // Presentational colour/font styles are accepted since task 2 (see
    // evernote_presentational_styles_map_to_marks_instead_of_blocking).
    for body in [
        "<table><tr><td>x</td></tr></table>",
        "<div><q>x</q></div>",
        "<div><a href=\"javascript:alert(1)\">x</a></div>",
        "<div><a href=\"https://host:bad/x\">x</a></div>",
        "<div><en-media hash=\"cccccccccccccccccccccccccccccccc\" type=\"image/png\"/></div>",
    ] {
        let enml = format!("<en-note>{body}</en-note>");
        assert!(
            matches!(
                convert_enml(&enml, &resources()),
                Err(EnmlFidelityBlocker { .. })
            ),
            "{body}"
        );
    }
    let ambiguous = BTreeMap::from([(
        IMAGE_MD5.into(),
        vec![
            resources()[IMAGE_MD5][0].clone(),
            resources()[IMAGE_MD5][0].clone(),
        ],
    )]);
    let enml = format!("<en-note><en-media hash=\"{IMAGE_MD5}\" type=\"image/png\"/></en-note>");
    assert!(convert_enml(&enml, &ambiguous).is_err());
    assert!(convert_enml(&enml, &BTreeMap::new()).is_err());
}

#[test]
fn converts_supported_heading_marks_and_ordinary_lists_without_flattening() {
    // Mutation caught: marks or ordinary list structure silently disappearing.
    let enml = "<en-note><h2>小标题</h2><div><u>下划线</u><s>删除</s><mark>重点</mark></div><ol><li>一</li><li>二</li></ol></en-note>";
    let result = convert_enml(enml, &BTreeMap::new()).unwrap();
    assert_eq!(
        result.html.as_str(),
        "<h2>小标题</h2><p><u>下划线</u><s>删除</s><mark>重点</mark></p><ol><li>一</li><li>二</li></ol>"
    );
    assert_eq!(
        result.search_text.as_str(),
        "小标题\n下划线删除重点\n一\n二"
    );
}

#[test]
fn malformed_xml_and_inline_checkbox_are_fidelity_blockers() {
    // Mutation caught: XML recovery and inline todo conversion to a lossy glyph.
    for enml in [
        "<en-note><div>x</en-note>",
        // A leading div checkbox is a checklist item since task 2; one in
        // the middle of a line is still not representable.
        "<en-note><div>x<en-todo checked=\"true\"/>y</div></en-note>",
        "<en-note><ul><li><en-todo checked=\"maybe\"/>x</li></ul></en-note>",
        "<en-note><?custom action?><div>x</div></en-note>",
        "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><en-note><div>x</div></en-note>",
        "<!DOCTYPE en-notebook><en-note><div>x</div></en-note>",
        "<!DOCTYPE en-note><!DOCTYPE en-note><en-note><div>x</div></en-note>",
    ] {
        assert!(convert_enml(enml, &BTreeMap::new()).is_err());
    }
}

#[test]
fn root_text_cdata_and_inline_media_keep_one_flow() {
    // Mutation caught: each root Text/CDATA callback becoming a separate paragraph.
    let plain = convert_enml("<en-note>前<![CDATA[后]]></en-note>", &BTreeMap::new()).unwrap();
    assert_eq!(plain.html.as_str(), "<p>前后</p>");
    assert_eq!(plain.search_text.as_str(), "前后");
    let enml = format!(
        "<en-note>甲<em>乙</em><en-media hash=\"{IMAGE_MD5}\" type=\"image/png\"/>丙</en-note>"
    );
    let mixed = convert_enml(&enml, &resources()).unwrap();
    assert_eq!(
        mixed.html.as_str(),
        format!("<p>甲<em>乙</em><img src=\":/{IMAGE_ID}\" alt=\"图.png\">丙</p>")
    );
    assert_eq!(mixed.search_text.as_str(), "甲乙图.png丙");
    let broken = convert_enml("<en-note>前<br/>后</en-note>", &BTreeMap::new()).unwrap();
    assert_eq!(broken.html.as_str(), "<p>前<br>后</p>");
    assert_eq!(broken.search_text.as_str(), "前\n后");
    let spaced = format!(
        "<en-note><en-media hash=\"{IMAGE_MD5}\" type=\"image/png\"/> <![CDATA[后]]></en-note>"
    );
    let spaced = convert_enml(&spaced, &resources()).unwrap();
    assert_eq!(
        spaced.html.as_str(),
        format!("<p><img src=\":/{IMAGE_ID}\" alt=\"图.png\"> 后</p>")
    );
}

#[test]
fn blocks_link_budget_overflow_and_linked_images_before_canonical_projection() {
    // Mutation caught: canonical projection silently removing a link mark or
    // a link surrounding an image while conversion still reports success.
    let href = format!("https://example.com/{}", "a".repeat(180));
    let links = (0..400)
        .map(|_| format!("<a href=\"{href}\">x</a>"))
        .collect::<String>();
    let enml = format!("<en-note><div>{links}</div></en-note>");
    assert!(convert_enml(&enml, &BTreeMap::new()).is_err());
    let linked = format!(
        "<en-note><div><a href=\"https://example.com\"><en-media hash=\"{IMAGE_MD5}\" type=\"image/png\"/></a></div></en-note>"
    );
    assert!(convert_enml(&linked, &resources()).is_err());
    let nested = "<en-note><div><a href=\"https://example.com/a\">甲<a href=\"https://example.com/b\">乙</a>丙</a></div></en-note>";
    assert!(convert_enml(nested, &BTreeMap::new()).is_err());
}

#[test]
fn a_div_containing_only_an_image_becomes_a_structural_image_block() {
    // Mutation caught: an isolated image being projected as a paragraph with
    // inline image instead of the editor's structural image block.
    let enml = format!(
        "<en-note><div><en-media hash=\"{IMAGE_MD5}\" type=\"image/png\"/></div></en-note>"
    );
    let converted = convert_enml(&enml, &resources()).unwrap();
    assert_eq!(
        converted.html.as_str(),
        format!("<img data-joplin-lite-block-image=\"true\" src=\":/{IMAGE_ID}\" alt=\"图.png\">")
    );
    assert!(matches!(converted.document.blocks(), [Block::Image { .. }]));
}

#[test]
fn semantic_nbsp_is_not_discarded_as_xml_formatting_whitespace() {
    // Mutation caught: Unicode trim treating NBSP as ignorable and dropping
    // visible text before choosing an image-only block or root paragraph.
    let root = convert_enml("<en-note>&#160;</en-note>", &BTreeMap::new()).unwrap();
    assert_eq!(root.html.as_str(), "<p>&nbsp;</p>");
    assert_eq!(root.search_text.as_str(), "\u{a0}");
    let enml = format!(
        "<en-note><div>&#160;<en-media hash=\"{IMAGE_MD5}\" type=\"image/png\"/></div></en-note>"
    );
    let converted = convert_enml(&enml, &resources()).unwrap();
    assert_eq!(
        converted.html.as_str(),
        format!("<p>&nbsp;<img src=\":/{IMAGE_ID}\" alt=\"图.png\"></p>")
    );
    assert_eq!(converted.search_text.as_str(), "\u{a0}图.png");
}

#[test]
fn media_blocker_path_uses_its_actual_child_index() {
    // Mutation caught: image-only optimization hard-coding /0 after skipped
    // XML formatting whitespace callbacks.
    let enml = "<en-note><div> \n<en-media hash=\"cccccccccccccccccccccccccccccccc\" type=\"image/png\"/></div></en-note>";
    let error = convert_enml(enml, &BTreeMap::new()).unwrap_err();
    assert_eq!(error.path, "/en-note/0/1");
}

#[test]
fn evernote_presentational_styles_map_to_marks_instead_of_blocking() {
    // Real Evernote ENML wraps almost every run in styled div/span/font.
    // Semantic styles and the text colour (Evernote's forecolor, parsed from
    // `color` styles and `<font color>`) become marks; font family and size
    // are not representable yet and are dropped rather than blocking.
    let enml = r##"<en-note><div style="text-align:left;font-family:Arial"><span style="font-weight: bold; color: rgb(0, 0, 0);">粗</span><span style="font-style:italic">斜</span><span style="text-decoration: underline;">下</span><span style="text-decoration:line-through">删</span><span style="--en-highlight:yellow;background-color: #ffef9e;">亮</span><font face="Arial" color="#333333">字</font><span style="font-size:14px">普通</span><span style="color:#FC1233;--inversion-type-color:simple">红</span></div></en-note>"##;
    let result = convert_enml(enml, &resources()).unwrap();
    assert_eq!(
        result.html.as_str(),
        "<p><span style=\"color: #000000\"><strong>粗</strong></span><em>斜</em><u>下</u><s>删</s><mark>亮</mark><span style=\"color: #333333\">字</span>普通<span style=\"color: #fc1233; --inversion-type-color: simple\">红</span></p>"
    );
    // Other color-string forms keep their colour and alpha; a value that is
    // not a colour is dropped instead of reaching the stored CSS.
    let forms = r#"<en-note><div><span style="color:hsl(120, 100%, 25%)">绿</span><font color="rebeccapurple">紫</font><span style="color:rgba(255, 0, 0, 0.5)">半</span><span style="color:hwb(240, 0%, 0%)">蓝</span><span style="color:currentcolor">当</span><font color="url(x)">坏</font></div></en-note>"#;
    assert_eq!(
        convert_enml(forms, &resources()).unwrap().html.as_str(),
        "<p><span style=\"color: #008000\">绿</span><span style=\"color: #663399\">紫</span><span style=\"color: rgba(255, 0, 0, 0.502)\">半</span><span style=\"color: #0000ff\">蓝</span>当坏</p>"
    );
}

#[test]
fn evernote_div_checkboxes_and_tel_links_convert() {
    // Evernote writes checkboxes as `<div><en-todo/>text</div>` rather than
    // inside `<ul><li>`, and phone numbers as `tel:` links.
    let enml = r#"<en-note><div><en-todo checked="true"/>已办</div><div><en-todo/>待办</div><div><a href="tel:+81-3-1234-5678">电话</a></div></en-note>"#;
    let result = convert_enml(enml, &resources()).unwrap();
    assert_eq!(
        result.html.as_str(),
        "<ul data-type=\"checklist\"><li data-checked=\"true\">已办</li><li data-checked=\"false\">待办</li></ul><p><a href=\"tel:+81-3-1234-5678\">电话</a></p>"
    );
}

#[test]
fn repeated_leading_checkboxes_collapse_and_hrefless_anchor_keeps_text() {
    let enml = r#"<en-note><div><en-todo checked="true"/><en-todo checked="true"/>重复</div><div><a name="x">锚点</a>文字</div></en-note>"#;
    let result = convert_enml(enml, &resources()).unwrap();
    assert_eq!(
        result.html.as_str(),
        "<ul data-type=\"checklist\"><li data-checked=\"true\">重复</li></ul><p>锚点文字</p>"
    );
    let conflicting = r#"<en-note><div><en-todo checked="true"/><en-todo/>冲突</div></en-note>"#;
    assert!(convert_enml(conflicting, &resources()).is_err());
}

#[test]
fn tel_links_accept_percent_encoded_separators_only() {
    let ok = r#"<en-note><div><a href="tel:(010)%2012345678">电话</a></div></en-note>"#;
    assert_eq!(
        convert_enml(ok, &resources()).unwrap().html.as_str(),
        "<p><a href=\"tel:(010)%2012345678\">电话</a></p>"
    );
    for bad in ["tel:12%zz", "tel:12%2", "tel:abc"] {
        let enml = format!(r#"<en-note><div><a href="{bad}">x</a></div></en-note>"#);
        assert!(convert_enml(&enml, &resources()).is_err(), "{bad}");
    }
}

#[test]
fn evernote_superscript_and_subscript_are_kept() {
    // Evernote textformatter/schema.ts parses `sup`/`sub` tags and
    // `vertical-align` styles into its superscript/subscript marks.
    let enml = r#"<en-note><div>H<sub>2</sub>O x<sup>2</sup><span style="vertical-align: super">上</span><span style="vertical-align:sub">下</span></div></en-note>"#;
    let result = convert_enml(enml, &resources()).unwrap();
    assert_eq!(
        result.html.as_str(),
        "<p>H<sub>2</sub>O x<sup>2上</sup><sub>下</sub></p>"
    );
}

#[test]
fn evernote_code_blocks_keep_language_lines_and_resources() {
    // common-editor codeblock/schema.ts parseENML: `div[style*="codeblock"]`
    // whose --en-codeblock is true (or white-space pre* with a monospace
    // font), and `pre`; the content is plain text lines (plaintext nodes
    // carry no marks) and syntaxLanguage comes from --en-syntaxLanguage.
    let enml = format!(
        r#"<en-note><div style="box-sizing: border-box; --en-codeblock:true; --en-syntaxLanguage:rust; font-family: monospace"><div>fn main() {{</div><div>    <span style="color:red"><b>x</b></span>();</div><div><br/></div><div>图<en-media hash="{IMAGE_MD5}" type="image/png"/></div><div>}}</div></div><div style="-en-codeblock:true">a<br/>b</div><div style="--en-codeblock:false; white-space: pre-wrap; font-family: Menlo, monospace">单行</div><pre>预&lt;格式&gt;</pre><div>后</div></en-note>"#
    );
    let result = convert_enml(&enml, &resources()).unwrap();
    assert_eq!(
        result.html.as_str(),
        format!(
            "<pre data-joplin-lite-block-code=\"true\" data-language=\"rust\">fn main() {{<br>&nbsp;&nbsp;&nbsp;&nbsp;x();<br><br>图<img src=\":/{IMAGE_ID}\" alt=\"图.png\"><br>}}</pre><pre data-joplin-lite-block-code=\"true\">a<br>b</pre><pre data-joplin-lite-block-code=\"true\">单行</pre><pre data-joplin-lite-block-code=\"true\">预&lt;格式&gt;</pre><p>后</p>"
        )
    );
    assert_eq!(
        result.resource_ids,
        vec![ResourceId::new(IMAGE_ID).unwrap()]
    );
    // Indentation survives, as the canonical model keeps visible spaces
    // (document.rs nbsp_tabs_and_newlines_have_deterministic_visible_model_forms).
    let Block::Code { inlines, .. } = &result.document.blocks()[0] else {
        panic!("code block");
    };
    assert!(inlines.iter().any(|inline| matches!(
        inline,
        app_lite_core::document::Inline::Text { text, .. } if text == "\u{a0}\u{a0}\u{a0}\u{a0}x();"
    )));

    // Not a code block without "codeblock" in its style; a language that is
    // not one name, or block structure inside, blocks instead of flattening.
    let monospace =
        r#"<en-note><div style="white-space: pre; font-family: monospace">x</div></en-note>"#;
    assert_eq!(
        convert_enml(monospace, &resources()).unwrap().html.as_str(),
        "<p>x</p>"
    );
    for enml in [
        r#"<en-note><div style="--en-codeblock:true; --en-syntaxLanguage:rust ignore">x</div></en-note>"#,
        r#"<en-note><div style="--en-codeblock:true"><table><tr><td>x</td></tr></table></div></en-note>"#,
        r#"<en-note><div style="--en-codeblock:true"><ul><li>x</li></ul></div></en-note>"#,
    ] {
        assert!(convert_enml(enml, &resources()).is_err(), "{enml}");
    }
}

#[test]
fn evernote_blockquote_imports_as_a_quote_container() {
    // common-editor quoteblock/schema.ts: parseENML takes `blockquote`, its
    // content `( p | todolist | ol | ul | h )+`.
    let enml = format!(
        r#"<en-note><blockquote><h2>要点</h2><div>说明<en-media hash="{IMAGE_MD5}" type="image/png"/></div><ul><li>一</li></ul><div><en-todo checked="true"/>已办</div></blockquote><div>外</div><blockquote><div>只有文字</div></blockquote></en-note>"#
    );
    let result = convert_enml(&enml, &resources()).unwrap();
    assert_eq!(
        result.html.as_str(),
        format!(
            "<blockquote data-joplin-lite-quote-container=\"true\"><h2>要点</h2><p>说明<img src=\":/{IMAGE_ID}\" alt=\"图.png\"></p><ul><li>一</li></ul><ul data-type=\"checklist\"><li data-checked=\"true\">已办</li></ul></blockquote><p>外</p><blockquote data-joplin-lite-block-quote=\"true\">只有文字</blockquote>"
        )
    );
    // Code, tables and nested quotes are not quoteblock content.
    for enml in [
        r#"<en-note><blockquote><div style="--en-codeblock:true">x</div></blockquote></en-note>"#,
        r#"<en-note><blockquote><table><tr><td>x</td></tr></table></blockquote></en-note>"#,
        r#"<en-note><blockquote><blockquote><div>x</div></blockquote></blockquote></en-note>"#,
    ] {
        assert!(convert_enml(enml, &resources()).is_err(), "{enml}");
    }
}

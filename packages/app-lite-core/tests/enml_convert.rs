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
fn blocks_unsupported_semantics_styles_and_unverified_media() {
    // Mutation caught: HTML recovery silently flattening a table/style, or
    // resolving media from outside the explicit same-note verified map.
    for body in [
        "<table><tr><td>x</td></tr></table>",
        "<div style=\"color:red\">x</div>",
        "<font color=\"red\">x</font>",
        "<div><sub>x</sub></div>",
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
        "<en-note><div><en-todo checked=\"true\"/>x</div></en-note>",
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

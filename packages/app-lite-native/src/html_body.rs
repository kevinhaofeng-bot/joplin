use html5ever::interface::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{Attribute, ParseOpts, QualName, local_name, ns, parse_fragment};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::{borrow::Cow, fmt};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HtmlBodyError {
    #[error("HTML parser failed")]
    Parse,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    pub fn from_blocks(blocks: Vec<Block>) -> Self {
        Self { blocks }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text { text: String, marks: Marks },
    SoftBreak,
    Image { resource_id: String, alt: String },
}

pub fn parse_html(input: &str) -> Result<Document, HtmlBodyError> {
    let sink = DomSink::default();
    let root = parse_fragment(
        sink,
        ParseOpts::default(),
        QualName::new(None, ns!(html), local_name!(body)),
        Vec::new(),
        false,
    )
    .from_utf8()
    .one(input.as_bytes());

    Ok(project_dom(&root))
}

pub fn serialize_html(document: &Document) -> String {
    if document.blocks.iter().all(block_is_empty) {
        return String::new();
    }
    let mut output = String::new();
    for block in &document.blocks {
        match block {
            Block::Paragraph(inlines) => {
                output.push_str("<p>");
                serialize_inlines(inlines, &mut output);
                output.push_str("</p>");
            }
        }
    }
    output
}

pub fn search_text(document: &Document) -> String {
    let mut output = String::new();
    for (index, block) in document.blocks.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        match block {
            Block::Paragraph(inlines) => {
                for inline in inlines {
                    match inline {
                        Inline::Text { text, .. } => output.push_str(text),
                        Inline::SoftBreak => output.push('\n'),
                        Inline::Image { alt, .. } => output.push_str(alt),
                    }
                }
            }
        }
    }
    output
}

pub fn resource_ids(document: &Document) -> Vec<String> {
    document
        .blocks
        .iter()
        .flat_map(|block| match block {
            Block::Paragraph(inlines) => inlines
                .iter()
                .filter_map(|inline| match inline {
                    Inline::Image { resource_id, .. }
                        if crate::body::validate_resource_id(resource_id).is_ok() =>
                    {
                        Some(resource_id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
        })
        .collect()
}

fn block_is_empty(block: &Block) -> bool {
    match block {
        Block::Paragraph(inlines) => inlines.iter().all(|inline| match inline {
            Inline::Text { text, .. } => text.is_empty(),
            Inline::SoftBreak => false,
            Inline::Image { .. } => false,
        }),
    }
}

fn serialize_inlines(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => serialize_text(text, marks, output),
            Inline::SoftBreak => output.push_str("<br>"),
            Inline::Image { resource_id, alt } => {
                if crate::body::validate_resource_id(resource_id).is_ok() {
                    output.push_str("<img src=\":/");
                    escape_attribute(resource_id, output);
                    output.push_str("\" alt=\"");
                    escape_attribute(alt, output);
                    output.push_str("\">");
                } else {
                    escape_text(alt, output);
                }
            }
        }
    }
}

fn serialize_text(text: &str, marks: &Marks, output: &mut String) {
    if marks.bold {
        output.push_str("<strong>");
    }
    if marks.italic {
        output.push_str("<em>");
    }
    if marks.underline {
        output.push_str("<u>");
    }
    escape_text(text, output);
    if marks.underline {
        output.push_str("</u>");
    }
    if marks.italic {
        output.push_str("</em>");
    }
    if marks.bold {
        output.push_str("</strong>");
    }
}

fn escape_text(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            ' ' => output.push_str("&nbsp;"),
            _ => output.push(character),
        }
    }
}

fn escape_attribute(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

#[derive(Clone)]
struct DomNode {
    data: DomData,
    children: RefCell<Vec<DomHandle>>,
    parent: RefCell<Option<Weak<DomNode>>>,
}

type DomHandle = Rc<DomNode>;

#[allow(dead_code)]
#[derive(Clone)]
enum DomData {
    Document,
    Doctype {
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    },
    Element {
        name: QualName,
        attrs: RefCell<Vec<Attribute>>,
        template_contents: Option<DomHandle>,
        mathml_annotation_xml_integration_point: bool,
    },
    Text(RefCell<StrTendril>),
    Comment(StrTendril),
    ProcessingInstruction {
        target: StrTendril,
        data: StrTendril,
    },
}

impl fmt::Debug for DomNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("DomNode").finish_non_exhaustive()
    }
}

struct DomSink {
    document: RefCell<Option<DomHandle>>,
    quirks_mode: Cell<QuirksMode>,
}

impl Default for DomSink {
    fn default() -> Self {
        Self {
            document: RefCell::new(None),
            quirks_mode: Cell::new(QuirksMode::NoQuirks),
        }
    }
}

impl DomSink {
    fn node(data: DomData) -> DomHandle {
        Rc::new(DomNode {
            data,
            children: RefCell::new(Vec::new()),
            parent: RefCell::new(None),
        })
    }

    fn parent(handle: &DomHandle) -> Option<DomHandle> {
        handle.parent.borrow().as_ref().and_then(Weak::upgrade)
    }

    fn detach(handle: &DomHandle) {
        let Some(parent) = Self::parent(handle) else {
            return;
        };
        parent
            .children
            .borrow_mut()
            .retain(|child| !Rc::ptr_eq(child, handle));
        *handle.parent.borrow_mut() = None;
    }

    fn set_parent(parent: &DomHandle, child: &DomHandle) {
        *child.parent.borrow_mut() = Some(Rc::downgrade(parent));
    }

    fn append_node(parent: &DomHandle, child: DomHandle) {
        Self::detach(&child);
        Self::set_parent(parent, &child);
        parent.children.borrow_mut().push(child);
    }

    fn append_text(parent: &DomHandle, text: StrTendril) {
        if text.is_empty() {
            return;
        }
        let mut children = parent.children.borrow_mut();
        if let Some(last) = children.last()
            && let DomData::Text(contents) = &last.data
        {
            contents.borrow_mut().push_tendril(&text);
            return;
        }
        let child = Self::node(DomData::Text(RefCell::new(text)));
        Self::set_parent(parent, &child);
        children.push(child);
    }

    fn append_to(parent: &DomHandle, child: NodeOrText<DomHandle>) {
        match child {
            NodeOrText::AppendNode(node) => Self::append_node(parent, node),
            NodeOrText::AppendText(text) => Self::append_text(parent, text),
        }
    }
}

impl TreeSink for DomSink {
    type Handle = DomHandle;
    type Output = DomHandle;
    type ElemName<'a> = &'a QualName;

    fn finish(self) -> Self::Output {
        self.document
            .into_inner()
            .expect("html5ever always creates a document node")
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Self::Handle {
        if let Some(document) = self.document.borrow().as_ref() {
            return document.clone();
        }
        let document = Self::node(DomData::Document);
        *self.document.borrow_mut() = Some(document.clone());
        document
    }

    fn elem_name<'a>(&'a self, target: &'a Self::Handle) -> Self::ElemName<'a> {
        match &target.data {
            DomData::Element { name, .. } => name,
            _ => panic!("elem_name called for a non-element node"),
        }
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: ElementFlags,
    ) -> Self::Handle {
        let template_contents = flags.template.then(|| Self::node(DomData::Document));
        Self::node(DomData::Element {
            name,
            attrs: RefCell::new(attrs),
            template_contents,
            mathml_annotation_xml_integration_point: flags.mathml_annotation_xml_integration_point,
        })
    }

    fn create_comment(&self, text: StrTendril) -> Self::Handle {
        Self::node(DomData::Comment(text))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Self::Handle {
        Self::node(DomData::ProcessingInstruction { target, data })
    }

    fn append(&self, parent: &Self::Handle, child: NodeOrText<Self::Handle>) {
        Self::append_to(parent, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &Self::Handle,
        prev_element: &Self::Handle,
        child: NodeOrText<Self::Handle>,
    ) {
        if let Some(parent) = Self::parent(element) {
            Self::append_to(&parent, child);
        } else {
            Self::append_to(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let document = self.get_document();
        Self::append_node(
            &document,
            Self::node(DomData::Doctype {
                name,
                public_id,
                system_id,
            }),
        );
    }

    fn get_template_contents(&self, target: &Self::Handle) -> Self::Handle {
        match &target.data {
            DomData::Element {
                template_contents: Some(contents),
                ..
            } => contents.clone(),
            _ => panic!("template contents requested for a non-template node"),
        }
    }

    fn same_node(&self, x: &Self::Handle, y: &Self::Handle) -> bool {
        Rc::ptr_eq(x, y)
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.quirks_mode.set(mode);
    }

    fn append_before_sibling(&self, sibling: &Self::Handle, new_node: NodeOrText<Self::Handle>) {
        let Some(parent) = Self::parent(sibling) else {
            return;
        };
        let mut children = parent.children.borrow_mut();
        let Some(index) = children.iter().position(|child| Rc::ptr_eq(child, sibling)) else {
            return;
        };
        match new_node {
            NodeOrText::AppendNode(node) => {
                Self::detach(&node);
                Self::set_parent(&parent, &node);
                children.insert(index, node);
            }
            NodeOrText::AppendText(text) => {
                if text.is_empty() {
                    return;
                }
                if index > 0
                    && let DomData::Text(contents) = &children[index - 1].data
                {
                    contents.borrow_mut().push_tendril(&text);
                } else {
                    let node = Self::node(DomData::Text(RefCell::new(text)));
                    Self::set_parent(&parent, &node);
                    children.insert(index, node);
                }
            }
        }
    }

    fn add_attrs_if_missing(&self, target: &Self::Handle, attrs: Vec<Attribute>) {
        let DomData::Element {
            attrs: existing, ..
        } = &target.data
        else {
            return;
        };
        let mut existing = existing.borrow_mut();
        for attr in attrs {
            if !existing.iter().any(|current| current.name == attr.name) {
                existing.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &Self::Handle) {
        Self::detach(target);
    }

    fn reparent_children(&self, node: &Self::Handle, new_parent: &Self::Handle) {
        let children = std::mem::take(&mut *node.children.borrow_mut());
        for child in children {
            *child.parent.borrow_mut() = None;
            Self::append_node(new_parent, child);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Self::Handle) -> bool {
        match &handle.data {
            DomData::Element {
                mathml_annotation_xml_integration_point,
                ..
            } => *mathml_annotation_xml_integration_point,
            _ => false,
        }
    }
}

fn project_dom(root: &DomHandle) -> Document {
    let mut projection = Projection::default();
    project_children(root, &mut projection, Marks::default(), false);
    projection.finish()
}

#[derive(Default)]
struct Projection {
    document: Document,
    current: Option<Vec<Inline>>,
}

impl Projection {
    fn finish(mut self) -> Document {
        self.flush();
        self.document
    }

    fn ensure_current(&mut self) -> &mut Vec<Inline> {
        self.current.get_or_insert_with(Vec::new)
    }

    fn flush(&mut self) {
        if let Some(inlines) = self.current.take() {
            self.document.blocks.push(Block::Paragraph(inlines));
        }
    }

    fn begin_block(&mut self) {
        self.flush();
        self.current = Some(Vec::new());
    }

    fn text(&mut self, text: &str, marks: &Marks, allow_formatting_whitespace: bool) {
        if text.is_empty() {
            return;
        }
        if !allow_formatting_whitespace && is_formatting_whitespace(text) {
            return;
        }
        let mut normalized = String::new();
        for character in text.chars() {
            match character {
                '\r' => normalized.push('\n'),
                '\t' => normalized.push_str("    "),
                '\u{00a0}' => normalized.push(' '),
                _ => normalized.push(character),
            }
        }
        let mut pieces = normalized.split('\n').peekable();
        while let Some(piece) = pieces.next() {
            if !piece.is_empty() {
                self.push_text(piece, marks);
            }
            if pieces.peek().is_some() {
                self.ensure_current().push(Inline::SoftBreak);
            }
        }
    }

    fn push_text(&mut self, text: &str, marks: &Marks) {
        let inlines = self.ensure_current();
        if let Some(Inline::Text {
            text: previous,
            marks: previous_marks,
        }) = inlines.last_mut()
            && previous_marks == marks
        {
            previous.push_str(text);
            return;
        }
        inlines.push(Inline::Text {
            text: text.to_owned(),
            marks: marks.clone(),
        });
    }

    fn image(&mut self, attrs: &[Attribute], marks: &Marks) {
        let source = attribute(attrs, "src");
        let alt = attribute(attrs, "alt").unwrap_or_default();
        let Some(source) = source else {
            self.text(&alt, marks, true);
            return;
        };
        let Some(resource_id) = source.strip_prefix(":/") else {
            self.text(&alt, marks, true);
            return;
        };
        if crate::body::validate_resource_id(resource_id).is_err() {
            self.text(&alt, marks, true);
            return;
        }
        self.ensure_current().push(Inline::Image {
            resource_id: resource_id.to_owned(),
            alt,
        });
    }
}

fn project_children(node: &DomHandle, projection: &mut Projection, marks: Marks, in_block: bool) {
    let children = node.children.borrow().clone();
    for child in children {
        project_node(&child, projection, marks.clone(), in_block);
    }
}

fn project_node(node: &DomHandle, projection: &mut Projection, marks: Marks, in_block: bool) {
    match &node.data {
        DomData::Text(text) => projection.text(&text.borrow(), &marks, in_block),
        DomData::Element { name, attrs, .. } => {
            let tag = name.local.to_string().to_ascii_lowercase();
            if matches!(tag.as_str(), "script" | "style" | "head" | "title") {
                return;
            }
            if is_block_element(&tag) {
                projection.begin_block();
                project_children(node, projection, marks, true);
                projection.flush();
                return;
            }
            if matches!(
                tag.as_str(),
                "div" | "section" | "article" | "header" | "footer"
            ) {
                projection.begin_block();
                project_children(node, projection, marks, true);
                projection.flush();
                return;
            }
            if tag == "br" {
                projection.ensure_current().push(Inline::SoftBreak);
                return;
            }
            if tag == "img" {
                projection.image(&attrs.borrow(), &marks);
                return;
            }
            let next_marks = Marks {
                bold: marks.bold || matches!(tag.as_str(), "strong" | "b"),
                italic: marks.italic || matches!(tag.as_str(), "em" | "i"),
                underline: marks.underline || tag == "u",
            };
            project_children(node, projection, next_marks, in_block);
        }
        DomData::Document
        | DomData::Doctype { .. }
        | DomData::Comment(_)
        | DomData::ProcessingInstruction { .. } => {
            project_children(node, projection, marks, in_block);
        }
    }
}

fn attribute(attrs: &[Attribute], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|attribute| attribute.name.local.to_string().eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value.to_string())
}

fn is_block_element(name: &str) -> bool {
    matches!(name, "p" | "h1" | "h2" | "h3" | "li" | "pre" | "blockquote")
}

fn is_formatting_whitespace(text: &str) -> bool {
    text.chars()
        .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESOURCE_ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn escapes_unicode_text_and_preserves_significant_whitespace() {
        let document = Document::from_blocks(vec![Block::Paragraph(vec![Inline::Text {
            text: "  中文 😀 & < > \" '  ".into(),
            marks: Marks::default(),
        }])]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<p>&nbsp;&nbsp;中文&nbsp;😀&nbsp;&amp;&nbsp;&lt;&nbsp;&gt;&nbsp;&quot;&nbsp;&#39;&nbsp;&nbsp;</p>"
        );
        assert_eq!(parse_html(&html).unwrap(), document);
    }

    #[test]
    fn paragraphs_marks_breaks_and_images_round_trip_in_order() {
        let document = Document::from_blocks(vec![
            Block::Paragraph(vec![
                Inline::Text {
                    text: "前".into(),
                    marks: Marks {
                        bold: true,
                        italic: false,
                        underline: false,
                    },
                },
                Inline::SoftBreak,
                Inline::Image {
                    resource_id: RESOURCE_ID.into(),
                    alt: "截图 & 证据.png".into(),
                },
                Inline::Text {
                    text: "后".into(),
                    marks: Marks {
                        bold: false,
                        italic: true,
                        underline: true,
                    },
                },
            ]),
            Block::Paragraph(Vec::new()),
        ]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<p><strong>前</strong><br><img src=\":/0123456789abcdef0123456789abcdef\" alt=\"截图 &amp; 证据.png\"><em><u>后</u></em></p><p></p>"
        );
        assert_eq!(parse_html(&html).unwrap(), document);
    }

    #[test]
    fn identical_documents_have_identical_html_bytes() {
        let document = Document::from_blocks(vec![Block::Paragraph(vec![Inline::Text {
            text: "稳定输出".into(),
            marks: Marks {
                bold: true,
                italic: true,
                underline: true,
            },
        }])]);
        assert_eq!(serialize_html(&document), serialize_html(&document));
        assert_eq!(
            serialize_html(&parse_html(&serialize_html(&document)).unwrap()),
            serialize_html(&document)
        );
    }

    #[test]
    fn only_local_valid_image_ids_survive_as_resource_nodes() {
        let html = format!(
            "<p><img src=\":/{RESOURCE_ID}\" alt=\"ok\"><img src=\"data:image/png;base64,AAAA\" alt=\"data\"><img src=\"https://example.com/a.png\" alt=\"remote\"><img src=\":/NOT-VALID\" alt=\"invalid\"></p>"
        );
        let document = parse_html(&html).unwrap();
        assert_eq!(resource_ids(&document), vec![RESOURCE_ID]);
        assert_eq!(
            document,
            Document::from_blocks(vec![Block::Paragraph(vec![
                Inline::Image {
                    resource_id: RESOURCE_ID.into(),
                    alt: "ok".into(),
                },
                Inline::Text {
                    text: "dataremoteinvalid".into(),
                    marks: Marks::default(),
                },
            ])])
        );
    }

    #[test]
    fn unsafe_elements_attributes_and_urls_are_downgraded_to_visible_text() {
        let document = parse_html(
            r#"<p onclick="alert(1)"><script>secret()</script><style>.x{display:none}</style><mark>visible</mark><a href="javascript:alert(1)">link</a></p>"#,
        )
        .unwrap();
        assert_eq!(search_text(&document), "visiblelink");
        assert!(!serialize_html(&document).contains("script"));
        assert!(!serialize_html(&document).contains("onclick"));
        assert!(!serialize_html(&document).contains("javascript:"));
    }

    #[test]
    fn search_projection_contains_visible_text_and_image_alt_but_not_markup() {
        let document = parse_html(&format!(
            "<p>正文 <strong>重点</strong><img src=\":/{RESOURCE_ID}\" alt=\"付款截图.png\"></p>"
        ))
        .unwrap();
        assert_eq!(search_text(&document), "正文 重点付款截图.png");
        assert!(!search_text(&document).contains(RESOURCE_ID));
        assert!(!search_text(&document).contains("strong"));
    }

    #[test]
    fn empty_semantic_content_serializes_to_empty_html() {
        assert_eq!(serialize_html(&Document::default()), "");
        assert_eq!(serialize_html(&Document::from_blocks(vec![])), "");
        assert_eq!(
            serialize_html(&Document::from_blocks(vec![Block::Paragraph(vec![])])),
            ""
        );
    }

    #[test]
    fn raw_text_elements_are_not_projected_as_markup() {
        let textarea = parse_html(&format!(
            r#"<textarea>before<img src=":/{RESOURCE_ID}" alt="fake">after</textarea>"#
        ))
        .unwrap();
        assert_eq!(
            search_text(&textarea),
            "before<img src=\":/0123456789abcdef0123456789abcdef\" alt=\"fake\">after"
        );
        assert!(resource_ids(&textarea).is_empty());

        let script = parse_html(
            r#"<p><strong>before<script>const x = '</strong>';</script>after</strong></p>"#,
        )
        .unwrap();
        assert_eq!(search_text(&script), "beforeafter");
        assert_eq!(
            script.blocks,
            vec![Block::Paragraph(vec![Inline::Text {
                text: "beforeafter".into(),
                marks: Marks {
                    bold: true,
                    ..Marks::default()
                }
            }])]
        );

        let style =
            parse_html(r#"<p>before<style>.x:after {content: '</p>';}</style>after</p>"#).unwrap();
        assert_eq!(style.blocks.len(), 1);
        assert_eq!(search_text(&style), "beforeafter");
    }

    #[test]
    fn html5_tree_builder_handles_malformed_blocks_and_marks() {
        let document = parse_html("<p><p>text</p>").unwrap();
        assert_eq!(
            document,
            Document::from_blocks(vec![
                Block::Paragraph(Vec::new()),
                Block::Paragraph(vec![Inline::Text {
                    text: "text".into(),
                    marks: Marks::default(),
                }]),
            ])
        );

        let nested = parse_html("<p><strong>one<p>two</strong>three").unwrap();
        assert_eq!(search_text(&nested), "one\ntwothree");
        assert_eq!(nested.blocks.len(), 2);
        assert!(matches!(
            nested.blocks[0],
            Block::Paragraph(ref inlines)
                if matches!(&inlines[0], Inline::Text { marks, .. } if marks.bold)
        ));
        assert!(matches!(
            nested.blocks[1],
            Block::Paragraph(ref inlines)
                if matches!(&inlines[0], Inline::Text { text, marks } if text == "two" && marks.bold)
                    && matches!(&inlines[1], Inline::Text { text, marks } if text == "three" && !marks.bold)
        ));
    }

    #[test]
    fn nbsp_tabs_and_newlines_have_deterministic_visible_model_forms() {
        let document = parse_html("<p>&nbsp;a\tb\r\nc&nbsp;</p>").unwrap();
        assert_eq!(
            document,
            Document::from_blocks(vec![Block::Paragraph(vec![
                Inline::Text {
                    text: " a    b".into(),
                    marks: Marks::default(),
                },
                Inline::SoftBreak,
                Inline::Text {
                    text: "c ".into(),
                    marks: Marks::default(),
                },
            ])])
        );
        assert_eq!(
            serialize_html(&document),
            "<p>&nbsp;a&nbsp;&nbsp;&nbsp;&nbsp;b<br>c&nbsp;</p>"
        );
        assert_eq!(
            parse_html("&nbsp;").map(|doc| search_text(&doc)).unwrap(),
            " "
        );
    }
}
